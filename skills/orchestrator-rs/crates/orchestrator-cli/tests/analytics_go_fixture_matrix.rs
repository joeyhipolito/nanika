//! Gate 3 (B3-DESIGN): byte-level analytics parity against Go-produced fixture
//! stores.
//!
//! Every expectation in this file is a *recorded observation of the accepted Go
//! binary*, captured by `tests/fixtures/analytics/regenerate.sh` over stores
//! that the same binary created. Nothing here is a hand-written expected
//! string, so the test cannot drift into asserting what the Rust code happens
//! to do.
//!
//! **A missing oracle is a hard failure, never a skip** (campaign rule). The
//! fixture loader panics with a regeneration hint rather than returning early,
//! which is the difference between this gate and `go_cli_differential`'s
//! opportunistic live comparison.
//!
//! Two halves, because the two surfaces have different composition rules:
//!
//! * **`audit scorecard`** runs end-to-end through the real binary. `audit_read`
//!   is enrolled in the production composition, so the whole path — argv
//!   parsing, home resolution, JSONL load, scorecard build, formatting — is
//!   under test and compared on stdout, stderr, *and* exit status.
//! * **`metrics`** cannot run end-to-end: B3-DESIGN Risk 4 keeps the production
//!   composition on `UnenrolledMetricsQueryService` until B5 fences the Go
//!   writer, so the binary deliberately fails closed. The gate therefore drives
//!   [`orchestrator_cli::run_metrics_with_service`], which parses the same full
//!   argument vector through the same `split_root_command` the binary uses and
//!   injects a storage owner. Rows are read out of the fixture `metrics.db`
//!   with SQL quoted from `internal/metrics/db.go`; the *typed query* those
//!   rows are fetched for is the one the CLI itself produced, recovered from
//!   `CapturingMetricsQueryService::recorded_calls`. A flag-parsing defect
//!   therefore still fails the gate, because it changes which rows the oracle
//!   SQL returns.
//!
//! Both halves assert the fixture store is byte-identical before and after —
//! these are read-only paths with no declared additive write — and that the
//! checked-in fixture itself is never touched (every case runs against a copy).

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use orchestrator_app::{
    CapturingMetricsQueryService, DayTrend, MetricsQueryCall, MetricsQueryFixtureResponses,
    MissionSummary, MissionsQuery, MissionsResult, PersonaMetric, PersonaMetricsResult, PhaseRow,
    PhasesQuery, PhasesResult, RoutingMethodDist, RoutingMethodsResult, SkillUsage,
    SkillUsageResult, TrendsQuery, TrendsResult,
};
use rusqlite::{Connection, OpenFlags};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------------------
// Fixture + oracle loading
// ---------------------------------------------------------------------------

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/analytics")
}

/// One recorded Go observation.
struct Oracle {
    argv: Vec<String>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: i32,
}

impl Oracle {
    /// Loads a recorded case.
    ///
    /// A missing capture is an `Err`, which fails the calling test — never a
    /// skip. `a_missing_oracle_is_a_hard_failure_rather_than_a_skip` checks
    /// that property directly.
    fn load(group: &str, name: &str) -> TestResult<Self> {
        let directory = fixtures_root().join("oracle").join(group).join(name);
        let read = |leaf: &str| -> TestResult<Vec<u8>> {
            fs::read(directory.join(leaf)).map_err(|error| {
                format!(
                    "missing Go oracle capture {}/{leaf}: {error}\n\
                     regenerate with: ORCHESTRATOR_ACCEPTED_GO_BIN=<go binary> \
                     crates/orchestrator-cli/tests/fixtures/analytics/regenerate.sh",
                    directory.display()
                )
                .into()
            })
        };
        let argv = String::from_utf8(read("argv")?)?
            .lines()
            .map(str::to_owned)
            .collect();
        let exit_code = String::from_utf8(read("exit")?)?.trim().parse()?;
        Ok(Self {
            argv,
            stdout: read("stdout")?,
            stderr: read("stderr")?,
            exit_code,
        })
    }
}

/// A disposable copy of a checked-in fixture store.
struct StoreCopy {
    root: PathBuf,
    source: PathBuf,
}

impl StoreCopy {
    fn of(store: &str) -> io::Result<Self> {
        let source = fixtures_root().join("stores").join(store);
        let root = std::env::temp_dir().join(format!(
            "orchestrator-cli-analytics-{}-{}",
            std::process::id(),
            NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root)?;
        for entry in fs::read_dir(&source)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                fs::copy(entry.path(), root.join(entry.file_name()))?;
            }
        }
        Ok(Self { root, source })
    }

    fn path(&self) -> &Path {
        &self.root
    }

    /// Byte image of every file in the working copy, so "unchanged" means
    /// unchanged bytes rather than an unchanged digest of a subset. Also
    /// catches SQLite side files (`-wal`, `-shm`, `-journal`) appearing, which
    /// a read-only path must not create.
    fn image(&self) -> io::Result<BTreeMap<String, Vec<u8>>> {
        image_of(&self.root)
    }

    /// The checked-in fixture's own image, asserted unchanged by every test.
    fn source_image(&self) -> io::Result<BTreeMap<String, Vec<u8>>> {
        image_of(&self.source)
    }
}

impl Drop for StoreCopy {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn image_of(directory: &Path) -> io::Result<BTreeMap<String, Vec<u8>>> {
    let mut image = BTreeMap::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            image.insert(
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path())?,
            );
        }
    }
    Ok(image)
}

// ---------------------------------------------------------------------------
// Half 1 — `audit scorecard`, end-to-end through the real binary
// ---------------------------------------------------------------------------

struct Observed {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: i32,
}

fn observe_binary(config_dir: &Path, home: &Path, argv: &[String]) -> io::Result<Observed> {
    let output = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .args(argv)
        .current_dir(config_dir)
        .env_clear()
        .env("HOME", home)
        .env("ORCHESTRATOR_CONFIG_DIR", config_dir)
        .env("PATH", "")
        .env("TMPDIR", config_dir)
        .output()?;
    Ok(Observed {
        stdout: output.stdout,
        stderr: output.stderr,
        exit_code: output.status.code().unwrap_or(-1),
    })
}

fn assert_audit_cases(store: &str, group: &str, cases: &[&str]) -> TestResult {
    let copy = StoreCopy::of(store)?;
    let source_before = copy.source_image()?;
    let before = copy.image()?;
    let home = copy.path().join("home");
    fs::create_dir_all(&home)?;

    for name in cases {
        let oracle = Oracle::load(group, name)?;
        let observed = observe_binary(copy.path(), &home, &oracle.argv)?;
        assert_eq!(
            String::from_utf8_lossy(&observed.stdout),
            String::from_utf8_lossy(&oracle.stdout),
            "stdout mismatch for {group}/{name} ({:?})",
            oracle.argv
        );
        assert_eq!(
            String::from_utf8_lossy(&observed.stderr),
            String::from_utf8_lossy(&oracle.stderr),
            "stderr mismatch for {group}/{name} ({:?})",
            oracle.argv
        );
        assert_eq!(
            observed.exit_code, oracle.exit_code,
            "exit status mismatch for {group}/{name} ({:?})",
            oracle.argv
        );
    }

    // `image_of` lists files only, so the `home` directory created above is
    // not part of either side of this comparison.
    assert_eq!(
        copy.image()?,
        before,
        "an audit read mutated its fixture store"
    );
    assert_eq!(
        copy.source_image()?,
        source_before,
        "an audit read reached the checked-in fixture"
    );
    Ok(())
}

#[test]
fn audit_scorecard_matches_the_go_oracle_over_a_populated_store() -> TestResult {
    // The store's third line is deliberately unparseable: Go's
    // `internal/audit/store.go:72-77` skips a malformed JSONL line silently
    // rather than failing the read, and the recorded oracle counts 3 audits
    // from 4 lines, which pins that behaviour.
    assert_audit_cases(
        "audits",
        "audits",
        &[
            "scorecard-text",
            "scorecard-json",
            "scorecard-domain-dev",
            "scorecard-last-1",
            // `--last 0` and `--last -1` are Go's "no limit" values.
            "scorecard-last-zero",
            "scorecard-last-negative",
        ],
    )
}

#[test]
fn audit_scorecard_matches_the_go_oracle_over_an_empty_store() -> TestResult {
    assert_audit_cases(
        "audits-empty",
        "audits-empty",
        &["scorecard-empty-text", "scorecard-empty-json"],
    )
}

#[test]
fn audit_scorecard_unknown_flag_rejection_pins_the_known_usage_text_gap() -> TestResult {
    // `--local` is not a Go flag. Go and Rust agree on the *decision* (reject,
    // exit 1, empty stdout); they disagree on the usage block written to
    // stderr, because Go's cobra renders the leaf command's usage and this port
    // renders the root's. That gap is declared in `src/cmd/mod.rs`'s module
    // documentation as an acknowledged follow-up, not a silent omission.
    //
    // This case pins the gap rather than hiding it: the day the leaf usage is
    // ported, the final assertion fails and this test is updated to a plain
    // equality check.
    let copy = StoreCopy::of("audits")?;
    let home = copy.path().join("home");
    fs::create_dir_all(&home)?;
    let oracle = Oracle::load("audits", "scorecard-local")?;
    let observed = observe_binary(copy.path(), &home, &oracle.argv)?;

    assert_eq!(
        observed.exit_code, oracle.exit_code,
        "rejection exit status"
    );
    assert_eq!(observed.stdout, oracle.stdout, "rejection stdout");
    assert!(observed.stdout.is_empty(), "a rejection wrote to stdout");

    let go_stderr = String::from_utf8_lossy(&oracle.stderr);
    let rust_stderr = String::from_utf8_lossy(&observed.stderr);
    assert!(
        go_stderr.contains("orchestrator audit scorecard [flags]"),
        "the Go oracle no longer renders leaf usage; regenerate the fixture"
    );
    assert!(
        rust_stderr.contains("orchestrator [command]"),
        "this port now renders something other than root usage: {rust_stderr}"
    );
    assert_ne!(
        rust_stderr, go_stderr,
        "leaf usage text now matches Go — replace this gap-pinning case with a \
         plain equality assertion and fold `scorecard-local` into the main matrix"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Half 2 — `metrics`, through the injected typed query service
// ---------------------------------------------------------------------------

fn open_read_only(database: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
    )
}

/// Ports `db.go:1232-1250` `QueryMissions`, including its `limit <= 0 -> 20`
/// default (`db.go:1222-1224`) and the `"ephemeral"` alias for the empty
/// `phases.worker_name` (`db.go:1227-1230`).
fn oracle_missions(
    connection: &Connection,
    query: &MissionsQuery,
) -> rusqlite::Result<MissionsResult> {
    let limit = if query.limit <= 0 { 20 } else { query.limit };
    let worker_match = if query.worker == "ephemeral" {
        ""
    } else {
        query.worker.as_str()
    };
    let mut statement = connection.prepare(
        "SELECT m.id, m.domain, m.status, m.decomp_source, m.task, m.duration_s,
                m.phases_total, m.phases_completed, m.phases_failed, m.started_at,
                COALESCE((
                    SELECT p.persona FROM phases p
                    WHERE p.mission_id = m.id AND p.persona != ''
                    ORDER BY rowid LIMIT 1
                ), '') AS top_persona
         FROM missions m
         WHERE (?1 = '' OR m.domain = ?1)
           AND (?2 = 0 OR m.started_at >= datetime('now', '-' || ?2 || ' days'))
           AND (?3 = '' OR m.status = ?3)
           AND (?4 = '' OR m.decomp_source = ?4)
           AND (?5 = '' OR EXISTS (
               SELECT 1 FROM phases p2
               WHERE p2.mission_id = m.id AND p2.worker_name = ?6
           ))
         ORDER BY m.started_at DESC
         LIMIT ?7",
    )?;
    let missions = statement
        .query_map(
            rusqlite::params![
                query.domain,
                query.days,
                query.status,
                query.decomp_source,
                query.worker,
                worker_match,
                limit,
            ],
            |row| {
                Ok(MissionSummary {
                    workspace_id: row.get(0)?,
                    domain: row.get(1)?,
                    status: row.get(2)?,
                    decomp_source: row.get(3)?,
                    task: row.get(4)?,
                    duration_sec: row.get(5)?,
                    phases_total: row.get(6)?,
                    phases_completed: row.get(7)?,
                    phases_failed: row.get(8)?,
                    started_at: row.get(9)?,
                    top_persona: row.get(10)?,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(MissionsResult { missions })
}

/// Ports `db.go:1278-1291` `QueryPersonaMetrics`.
fn oracle_personas(connection: &Connection) -> rusqlite::Result<PersonaMetricsResult> {
    let mut statement = connection.prepare(
        "SELECT persona,
                COUNT(*) AS phase_count,
                AVG(CAST(duration_s AS REAL)) AS avg_duration,
                SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) * 100.0 / COUNT(*) AS failure_rate,
                AVG(CAST(retries AS REAL)) AS avg_retries,
                SUM(CASE WHEN selection_method = 'llm' THEN 1 ELSE 0 END) * 100.0 / COUNT(*) AS llm_pct,
                SUM(CASE WHEN selection_method = 'keyword' THEN 1 ELSE 0 END) * 100.0 / COUNT(*) AS keyword_pct
         FROM phases
         WHERE persona != ''
         GROUP BY persona
         ORDER BY phase_count DESC",
    )?;
    let personas = statement
        .query_map([], |row| {
            Ok(PersonaMetric {
                persona: row.get(0)?,
                phase_count: row.get(1)?,
                avg_duration_sec: row.get(2)?,
                failure_rate: row.get(3)?,
                avg_retries: row.get(4)?,
                llm_pct: row.get(5)?,
                keyword_pct: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(PersonaMetricsResult { personas })
}

/// Ports `db.go:1313-1320` `QuerySkillUsage`.
fn oracle_skills(connection: &Connection) -> rusqlite::Result<SkillUsageResult> {
    let mut statement = connection.prepare(
        "SELECT skill_name, phase, persona, source, COUNT(*) AS invocations
         FROM skill_invocations
         GROUP BY skill_name, phase, persona, source
         ORDER BY invocations DESC, skill_name, phase
         LIMIT 100",
    )?;
    let skills = statement
        .query_map([], |row| {
            Ok(SkillUsage {
                skill_name: row.get(0)?,
                phase: row.get(1)?,
                persona: row.get(2)?,
                source: row.get(3)?,
                invocations: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(SkillUsageResult { skills })
}

/// Ports `db.go:1341-1352` `QueryTrends`, including its `days <= 0 -> 30`
/// default.
fn oracle_trends(connection: &Connection, query: &TrendsQuery) -> rusqlite::Result<TrendsResult> {
    let days = if query.days <= 0 { 30 } else { query.days };
    let mut statement = connection.prepare(
        "SELECT date(started_at) AS day,
                COUNT(*) AS total,
                SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) AS successes,
                AVG(CAST(duration_s AS REAL)) AS avg_duration
         FROM missions
         WHERE started_at >= datetime('now', '-' || ?1 || ' days')
         GROUP BY day
         ORDER BY day DESC",
    )?;
    let trends = statement
        .query_map([days], |row| {
            Ok(DayTrend {
                day: row.get(0)?,
                total: row.get(1)?,
                successes: row.get(2)?,
                avg_duration: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(TrendsResult { trends })
}

/// Ports `db.go:1167-1183` `QueryRoutingMethodDistribution`.
fn oracle_routing_methods(connection: &Connection) -> rusqlite::Result<RoutingMethodsResult> {
    let mut statement = connection.prepare(
        "SELECT COALESCE(NULLIF(selection_method, ''), 'unknown') AS method,
                COUNT(*) AS cnt,
                COALESCE(
                    COUNT(*) * 100.0 / NULLIF((
                        SELECT COUNT(*) FROM phases
                        WHERE persona != ''
                          AND COALESCE(selection_method, '') != 'required_review'
                    ), 0),
                0.0) AS pct
         FROM phases
         WHERE persona != ''
           AND COALESCE(selection_method, '') != 'required_review'
         GROUP BY method
         ORDER BY cnt DESC",
    )?;
    let methods = statement
        .query_map([], |row| {
            Ok(RoutingMethodDist {
                method: row.get(0)?,
                count: row.get(1)?,
                pct: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(RoutingMethodsResult { methods })
}

/// Ports `db.go:1387-1392` `QueryPhases` and `unmarshalSkillsJSON`
/// (`db.go:1425-1434`), whose JSON-decode failure yields *no* skills rather
/// than an error — the fixture's `not-json` row pins that.
fn oracle_phases(connection: &Connection, query: &PhasesQuery) -> rusqlite::Result<PhasesResult> {
    let mut statement = connection.prepare(
        "SELECT name, persona, status, duration_s, COALESCE(parsed_skills, '')
         FROM phases
         WHERE mission_id = ?1
         ORDER BY rowid",
    )?;
    let phases = statement
        .query_map([&query.workspace_id], |row| {
            let raw: String = row.get(4)?;
            Ok(PhaseRow {
                name: row.get(0)?,
                persona: row.get(1)?,
                status: row.get(2)?,
                duration_s: row.get(3)?,
                parsed_skills: serde_json::from_str::<Vec<String>>(&raw).unwrap_or_default(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(PhasesResult { phases })
}

/// Runs one recorded `metrics` case.
///
/// Pass 1 lets the CLI parse the argument vector and record the typed query it
/// derived. Pass 2 answers that exact query out of the fixture database and
/// renders. Parsing is therefore genuinely under test: a wrong `--last`, a
/// dropped `--domain`, or a mis-routed subcommand changes the recorded query,
/// which changes the rows, which breaks the byte comparison against Go.
fn assert_metrics_case(database: &Path, group: &str, name: &str) -> TestResult {
    let oracle = Oracle::load(group, name)?;

    let probe = CapturingMetricsQueryService::default();
    let mut discarded = Vec::new();
    orchestrator_cli::run_metrics_with_service(oracle.argv.clone(), &probe, &mut discarded)?;
    let calls = probe.recorded_calls();
    assert_eq!(
        calls.len(),
        1,
        "{group}/{name} issued {} queries, expected exactly one",
        calls.len()
    );

    // A WAL-mode database cannot be opened SQLITE_OPEN_READ_ONLY without its
    // -shm/-wal side files, so `regenerate.sh` converts each fixture to the
    // rollback journal after its last Go invocation. Name the file in the error
    // so a regression there reads as a fixture fault, not a query fault.
    let connection = open_read_only(database)
        .map_err(|error| format!("opening {}: {error}", database.display()))?;
    let mut responses = MetricsQueryFixtureResponses::default();
    match calls.first().ok_or("no recorded query")? {
        MetricsQueryCall::Missions(query) => {
            responses.missions = oracle_missions(&connection, query)?;
        }
        MetricsQueryCall::PersonaMetrics(_) => responses.personas = oracle_personas(&connection)?,
        MetricsQueryCall::SkillUsage(_) => responses.skills = oracle_skills(&connection)?,
        MetricsQueryCall::Trends(query) => responses.trends = oracle_trends(&connection, query)?,
        MetricsQueryCall::RoutingMethods(_) => {
            responses.routing_methods = oracle_routing_methods(&connection)?;
        }
        MetricsQueryCall::Phases(query) => responses.phases = oracle_phases(&connection, query)?,
    }
    drop(connection);

    let service = CapturingMetricsQueryService::new(responses);
    let mut rendered = Vec::new();
    orchestrator_cli::run_metrics_with_service(oracle.argv.clone(), &service, &mut rendered)?;

    assert_eq!(
        String::from_utf8_lossy(&rendered),
        String::from_utf8_lossy(&oracle.stdout),
        "stdout mismatch for {group}/{name} ({:?})",
        oracle.argv
    );
    assert_eq!(oracle.exit_code, 0, "{group}/{name} is not a success case");
    assert!(
        oracle.stderr.is_empty(),
        "{group}/{name} wrote to stderr in Go"
    );
    Ok(())
}

fn assert_metrics_cases(store: &str, group: &str, cases: &[&str]) -> TestResult {
    let copy = StoreCopy::of(store)?;
    let source_before = copy.source_image()?;
    let before = copy.image()?;
    let database = copy.path().join("metrics.db");

    for name in cases {
        assert_metrics_case(&database, group, name)?;
    }

    assert_eq!(
        copy.image()?,
        before,
        "a metrics read mutated its fixture store (or created a SQLite side file)"
    );
    assert_eq!(
        copy.source_image()?,
        source_before,
        "a metrics read reached the checked-in fixture"
    );
    Ok(())
}

#[test]
fn metrics_matches_the_go_oracle_over_a_populated_store() -> TestResult {
    assert_metrics_cases(
        "populated",
        "populated",
        &[
            "missions-default",
            "missions-last-2",
            // Go's `QueryMissions` maps a non-positive `--last` to 20.
            "missions-last-zero",
            "missions-last-negative",
            "missions-domain-dev",
            "missions-status-failed",
            "missions-decomp-source",
            "missions-worker-alpha",
            "missions-worker-ephemeral",
            "missions-days-wide",
            "personas",
            "skills",
            "trends-wide",
            "routing-methods",
            "phases-alpha",
            "phases-missing",
        ],
    )
}

#[test]
fn metrics_matches_the_go_oracle_over_an_empty_store() -> TestResult {
    assert_metrics_cases(
        "empty",
        "empty",
        &[
            "missions-empty",
            "personas-empty",
            "skills-empty",
            // Go prints the *raw* `--days` in the empty message even though the
            // query clamps a non-positive value to 30 (`metrics.go:238-244`).
            "trends-empty-zero-days",
            "routing-methods-empty",
            "phases-empty",
        ],
    )
}

#[test]
fn routing_method_alert_threshold_is_pinned_from_both_sides() -> TestResult {
    // The `populated` store's fallback share is 40% and its oracle carries the
    // ALERT line; the `boundary` store's is exactly 30.0% and its oracle does
    // not, because Go alerts only on a *strictly greater* rate. Asserting both
    // directions is what makes the boundary meaningful — a `>=` regression
    // passes the first case and fails the second.
    let above = Oracle::load("populated", "routing-methods")?;
    let at = Oracle::load("boundary", "routing-methods-boundary")?;
    assert!(
        String::from_utf8_lossy(&above.stdout).contains("ALERT: fallback routing rate 40.0%"),
        "the populated oracle no longer alerts; regenerate the fixture"
    );
    assert!(
        String::from_utf8_lossy(&at.stdout).contains("fallback                     3     30.0%"),
        "the boundary oracle is no longer at exactly 30.0%; regenerate the fixture"
    );
    assert!(
        !String::from_utf8_lossy(&at.stdout).contains("ALERT"),
        "Go now alerts at exactly the threshold; the port must follow"
    );

    assert_metrics_cases("boundary", "boundary", &["routing-methods-boundary"])
}

#[test]
fn a_missing_oracle_is_a_hard_failure_rather_than_a_skip() {
    // Vacuity check for this file's central campaign rule. If `Oracle::load`
    // ever degrades to returning a default or skipping, every assertion above
    // becomes theatre, so the failure mode itself is tested.
    assert!(
        Oracle::load("populated", "no-such-case").is_err(),
        "a missing oracle capture did not fail the gate"
    );
    // ...and a present one still loads, so the check above is not passing
    // because the loader is simply broken.
    assert!(
        Oracle::load("populated", "personas").is_ok(),
        "the loader rejects a capture that exists"
    );
}
