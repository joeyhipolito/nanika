//! Gate 4 (B3-DESIGN): the learning-maintenance dry-run / `--apply` contract.
//!
//! What this gate proves, over a real Go-produced `learnings.db`:
//!
//! 1. **Dry-run is the default.** No command in the family writes, or even
//!    reaches a write path, without an explicit `--apply`.
//! 2. **`--apply` is refused before the store is touched.** Proven with a
//!    tripwire reader that counts and refuses every access: a refusal ordered
//!    after a read would raise the counter *and* change the error the caller
//!    sees.
//! 3. **Dry-run is byte-stable.** Repeated runs produce identical bytes and
//!    leave the store's [`RowSetDigest`] unchanged.
//! 4. **The one declared additive edge changes only its declared records.**
//!    `GoLearningAppender::insert_new` under an `AdditiveFixtureGrant` grows the
//!    id set by exactly the declared ids and leaves the pre-existing rows'
//!    digest byte-identical — checked with `RowSetDigest::restricted_to`, and
//!    backed by a mutation check proving that assertion has teeth.
//! 5. **The flag surface matches the accepted Go binary**, pinned against
//!    Go's own `--help` output rather than against this file's memory of it.
//!
//! 6. **The dry-run data lines are byte-identical to Go's** (TRK-1248).
//!    [`every_frozen_go_dry_run_capture_is_reproduced_byte_for_byte`] replays
//!    every capture under `tests/fixtures/learning/oracle/` — argv read from
//!    the capture itself, not from this file's memory of it — and compares
//!    stdout bytes and exit status against the frozen Go observation. This was
//!    previously a pinned *inverted* assertion (the port was asserted **not**
//!    to emit those lines) because B3-DESIGN §3.1 had not ported
//!    `used_count`, `seen_count`, `injection_count`, `compliance_rate`, or
//!    `embedding`-null state; those columns have since landed, read-only, on
//!    `LearningRow` / `LearningStats`.

use std::os::unix::fs::PermissionsExt;
use std::{
    cell::Cell,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use orchestrator_app::{
    AdditiveFixtureGrant, GoAdapterError, GoLearningAdapter, GoLearningAppender, GoLearningReader,
    IsolatedFixtureRoot, LearningListQuery, LearningPage, LearningRow, LearningStats,
    NewLearningRow, RowSetDigest, TopQualityQuery,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/learning")
}

/// A fresh fixture root holding its own copy of the checked-in learning store,
/// so subcases cannot interfere with one another.
struct Fixture {
    parent: PathBuf,
    root: IsolatedFixtureRoot,
}

impl Fixture {
    fn create() -> TestResult<Self> {
        let parent = std::env::temp_dir().join(format!(
            "orchestrator-cli-learning-{}-{}",
            std::process::id(),
            NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&parent)?;
        let root = IsolatedFixtureRoot::create_fresh(&parent)?;

        let source = fixtures_root().join("store/.alluka/learnings.db");
        if !source.is_file() {
            return Err(format!(
                "missing checked-in learning store {}\n\
                 regenerate with: ORCHESTRATOR_ACCEPTED_GO_BIN=<go binary> \
                 crates/orchestrator-cli/tests/fixtures/learning/regenerate.sh",
                source.display()
            )
            .into());
        }
        let target_dir = root.path().join(".alluka");
        fs::create_dir_all(&target_dir)?;
        let database = target_dir.join("learnings.db");
        fs::copy(&source, &database)?;
        // `fs::copy` carries the source's permission bits, and under the
        // verification lease the checked-in fixture lives in a `chmod -R a-w`
        // source snapshot. Without this the scratch copy is read-only and
        // every write path in this matrix fails with SQLITE_READONLY.
        let mut permissions = fs::metadata(&database)?.permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&database, permissions)?;
        Ok(Self { parent, root })
    }

    fn reader(&self) -> Result<GoLearningAdapter, GoAdapterError> {
        GoLearningAdapter::in_fixture(&self.root)
    }

    fn grant(&self) -> TestResult<AdditiveFixtureGrant> {
        Ok(AdditiveFixtureGrant::in_fixture(&self.root)?)
    }

    fn database(&self) -> PathBuf {
        self.root.path().join(".alluka/learnings.db")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

/// One frozen Go observation: the argv it was captured with, the bytes it
/// printed, and the status it exited with. Missing captures are a hard failure.
struct Oracle {
    name: String,
    argv: Vec<String>,
    stdout: String,
    exit: i32,
}

impl Oracle {
    /// A missing capture is an `Err`, which fails the calling test — never a
    /// skip.
    fn load(name: &str) -> TestResult<Self> {
        let directory = fixtures_root().join("oracle").join(name);
        let read = |file: &str| -> TestResult<String> {
            let path = directory.join(file);
            Ok(fs::read_to_string(&path).map_err(|error| {
                format!(
                    "missing Go oracle capture {}: {error}\n\
                     regenerate with: ORCHESTRATOR_ACCEPTED_GO_BIN=<go binary> \
                     crates/orchestrator-cli/tests/fixtures/learning/regenerate.sh",
                    path.display()
                )
            })?)
        };
        // `argv` is one token per line, so the replay cannot drift from what Go
        // was actually handed.
        let argv = read("argv")?
            .lines()
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        let stdout = read("stdout")?;
        let exit = read("exit")?.trim().parse::<i32>()?;
        Ok(Self {
            name: name.to_owned(),
            argv,
            stdout,
            exit,
        })
    }

    /// Every captured command case, in a stable order. `help` holds the
    /// `--help` captures used by the flag-surface tests, not command runs.
    fn all_command_cases() -> TestResult<Vec<Self>> {
        let mut names: Vec<String> = fs::read_dir(fixtures_root().join("oracle"))?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name != "help")
            .collect();
        names.sort();
        names.iter().map(|name| Self::load(name)).collect()
    }
}

/// A reader that records any access and refuses it.
///
/// This is how "refused before the store is touched" is proven rather than
/// asserted, and it gives two independent signals: the access counter stays at
/// zero, *and* the error the caller sees is the write refusal rather than this
/// reader's `Read` failure. A refusal ordered after a read would flip both.
#[derive(Default)]
struct TripwireReader {
    touched: Cell<usize>,
}

impl TripwireReader {
    fn touches(&self) -> usize {
        self.touched.get()
    }

    fn trip<T>(&self, operation: &'static str) -> Result<T, GoAdapterError> {
        self.touched.set(self.touched.get() + 1);
        Err(GoAdapterError::Read { operation })
    }
}

impl GoLearningReader for TripwireReader {
    fn schema_version(&self) -> Result<u32, GoAdapterError> {
        self.trip("tripwire: schema_version")
    }
    fn list(&self, _query: &LearningListQuery) -> Result<LearningPage, GoAdapterError> {
        self.trip("tripwire: list")
    }
    fn top_by_quality(&self, _query: &TopQualityQuery) -> Result<Vec<LearningRow>, GoAdapterError> {
        self.trip("tripwire: top_by_quality")
    }
    fn stats(&self) -> Result<LearningStats, GoAdapterError> {
        self.trip("tripwire: stats")
    }
    fn row_set_digest(&self) -> Result<RowSetDigest, GoAdapterError> {
        self.trip("tripwire: row_set_digest")
    }
    fn digest_components(&self) -> Result<Vec<(String, String)>, GoAdapterError> {
        self.trip("tripwire: digest_components")
    }
}

/// Every dry-run invocation in the matrix.
const DRY_RUN_CASES: &[&[&str]] = &[
    &["stats"],
    &["prune"],
    &["prune", "--max-age", "30", "--min-score", "0.5"],
    &["prune", "--max-age=30", "--min-score=0.5", "--max-count=1"],
    &["archive"],
    &["archive", "--domain", "work"],
    &["backfill-embeddings"],
    &["backfill-embeddings", "--limit", "2", "--batch-size", "1"],
    &["backfill-embeddings", "--since=720h", "--include-archived"],
];

/// Every `--apply` invocation in the matrix.
const APPLY_CASES: &[&[&str]] = &[
    &["prune", "--apply"],
    &["prune", "--apply=true", "--min-score=0.9"],
    &["archive", "--apply"],
    &["archive", "--apply", "--domain", "dev"],
    &["backfill-embeddings", "--apply"],
    &["backfill-embeddings", "--apply", "--limit=1"],
];

fn invoke<R: GoLearningReader + ?Sized>(
    reader: &R,
    argv: &[&str],
) -> (Vec<u8>, Result<(), String>) {
    let mut output = Vec::new();
    let result = orchestrator_cli::run_learning_with_reader(argv.to_vec(), reader, &mut output)
        .map_err(|error| error.to_string());
    (output, result)
}

// ---------------------------------------------------------------------------
// 1 + 2. Dry-run default; `--apply` refused before any store access
// ---------------------------------------------------------------------------

#[test]
fn apply_is_refused_before_the_store_is_opened() -> TestResult {
    for argv in APPLY_CASES {
        let reader = TripwireReader::default();
        let (output, result) = invoke(&reader, argv);
        let message = result
            .err()
            .ok_or(format!("{argv:?} did not refuse --apply"))?;
        assert!(
            message.contains("--apply is not ported"),
            "{argv:?} failed for the wrong reason: {message}"
        );
        assert_eq!(
            reader.touches(),
            0,
            "{argv:?} touched the store before refusing"
        );
        assert!(
            output.is_empty(),
            "{argv:?} wrote output before refusing: {}",
            String::from_utf8_lossy(&output)
        );
    }
    Ok(())
}

#[test]
fn the_tripwire_reader_actually_trips() -> TestResult {
    // Vacuity check for the test above: if `TripwireReader` silently succeeded,
    // a zero touch count would prove nothing. A dry run must trip it.
    let reader = TripwireReader::default();
    let (_, result) = invoke(&reader, &["prune"]);
    assert!(result.is_err());
    assert_eq!(reader.touches(), 1, "the tripwire never fired on a dry run");
    Ok(())
}

#[test]
fn dry_run_is_the_default_and_reaches_the_read_path() -> TestResult {
    let fixture = Fixture::create()?;
    let reader = fixture.reader()?;
    for argv in DRY_RUN_CASES {
        let (output, result) = invoke(&reader, argv);
        // The distinguishing assertion: without `--apply` the command renders
        // its dry-run lines. A regression that made dry-run imply apply would
        // surface here as the write refusal instead.
        result.map_err(|message| format!("{argv:?} failed: {message}"))?;
        let rendered = String::from_utf8(output)?;
        assert!(!rendered.is_empty(), "{argv:?} rendered nothing");
        assert!(
            !rendered.contains("would remove") || rendered.contains("dry-run"),
            "{argv:?} rendered a removal line outside a dry-run frame: {rendered}"
        );
    }
    Ok(())
}

#[test]
fn no_invocation_in_the_matrix_changes_the_store() -> TestResult {
    let fixture = Fixture::create()?;
    let reader = fixture.reader()?;
    let before_digest = reader.row_set_digest()?;
    let before_bytes = fs::read(fixture.database())?;

    for argv in DRY_RUN_CASES.iter().chain(APPLY_CASES.iter()) {
        let _ = invoke(&reader, argv);
    }

    assert_eq!(
        reader.row_set_digest()?,
        before_digest,
        "the matrix changed the row set"
    );
    assert_eq!(
        fs::read(fixture.database())?,
        before_bytes,
        "the matrix changed learnings.db on disk"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 3. Dry-run byte stability
// ---------------------------------------------------------------------------

#[test]
fn repeated_dry_runs_are_byte_identical() -> TestResult {
    let fixture = Fixture::create()?;
    let reader = fixture.reader()?;
    for argv in DRY_RUN_CASES {
        let first = invoke(&reader, argv);
        let digest_between = reader.row_set_digest()?;
        let second = invoke(&reader, argv);
        assert_eq!(first.0, second.0, "{argv:?} stdout is not byte-stable");
        assert_eq!(first.1, second.1, "{argv:?} outcome is not stable");
        assert_eq!(
            reader.row_set_digest()?,
            digest_between,
            "{argv:?} changed the row set between runs"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 4. The one declared additive edge
// ---------------------------------------------------------------------------

fn new_row(id: &str) -> NewLearningRow {
    NewLearningRow {
        id: id.to_owned(),
        learning_type: "insight".to_owned(),
        content: format!("{id} additive content"),
        context: format!("{id} additive context"),
        domain: "dev".to_owned(),
        created_at: "2026-08-26T00:00:00Z".to_owned(),
    }
}

#[test]
fn the_declared_additive_write_changes_only_its_declared_records() -> TestResult {
    let fixture = Fixture::create()?;
    let reader = fixture.reader()?;
    let grant = fixture.grant()?;
    let appender = GoLearningAppender::for_grant(&grant)?;

    let before = reader.row_set_digest()?;
    let components_before = reader.digest_components()?;

    // Self-check the comparison method before relying on it: restricting a
    // digest to its own full id set must reproduce the digest, otherwise the
    // "pre-existing portion unchanged" assertion below would be meaningless.
    assert_eq!(
        before.restricted_to(before.ids(), &components_before),
        before.digest(),
        "RowSetDigest::restricted_to does not reproduce the full digest"
    );

    let declared = ["l-900-additive", "l-901-additive"];
    let receipt = appender.insert_new(
        &grant,
        &declared.iter().map(|id| new_row(id)).collect::<Vec<_>>(),
    )?;
    assert_eq!(receipt.inserted_ids, declared);

    let after = reader.row_set_digest()?;
    let components_after = reader.digest_components()?;

    // The id set grew by exactly the declared ids.
    let mut expected_ids = before.ids().to_vec();
    expected_ids.extend(declared.iter().map(|id| (*id).to_owned()));
    expected_ids.sort();
    let mut observed_ids = after.ids().to_vec();
    observed_ids.sort();
    assert_eq!(
        observed_ids, expected_ids,
        "the id set changed unexpectedly"
    );

    // The pre-existing rows are byte-identical.
    assert_eq!(
        after.restricted_to(before.ids(), &components_after),
        before.digest(),
        "the additive write disturbed a pre-existing row"
    );
    assert_ne!(
        after.digest(),
        before.digest(),
        "the additive write did not change the whole-set digest at all"
    );
    Ok(())
}

#[test]
fn the_restricted_digest_detects_a_disturbed_pre_existing_row() -> TestResult {
    // Mutation check for the assertion above. Without this, a
    // `restricted_to` that ignored its `source` argument would let the additive
    // test pass no matter what the write did.
    let fixture = Fixture::create()?;
    let reader = fixture.reader()?;
    let before = reader.row_set_digest()?;

    let connection = rusqlite::Connection::open(fixture.database())?;
    connection.execute(
        "UPDATE learnings SET quality_score = quality_score + 1.0 WHERE id = 'l-001'",
        [],
    )?;
    drop(connection);

    let after = reader.row_set_digest()?;
    let components_after = reader.digest_components()?;
    assert_eq!(after.ids(), before.ids(), "the mutation changed the id set");
    assert_ne!(
        after.restricted_to(before.ids(), &components_after),
        before.digest(),
        "a mutated pre-existing row went undetected — the additive-write \
         assertion has no teeth"
    );
    Ok(())
}

#[test]
fn a_colliding_id_is_refused_rather_than_overwriting() -> TestResult {
    let fixture = Fixture::create()?;
    let reader = fixture.reader()?;
    let grant = fixture.grant()?;
    let appender = GoLearningAppender::for_grant(&grant)?;

    let before = reader.row_set_digest()?;
    let existing = before
        .ids()
        .first()
        .cloned()
        .ok_or("the fixture store is empty")?;

    let result = appender.insert_new(&grant, &[new_row(&existing)]);
    assert!(
        result.is_err(),
        "an id collision was accepted, so the write is not purely additive"
    );
    assert_eq!(
        reader.row_set_digest()?,
        before,
        "a refused insert still changed the row set"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 5. Flag surface, pinned against the accepted Go binary
// ---------------------------------------------------------------------------

fn go_help(command: &str) -> TestResult<String> {
    let path = fixtures_root().join("oracle/help").join(command);
    Ok(fs::read_to_string(&path).map_err(|error| {
        format!(
            "missing Go help capture {}: {error}\n\
             regenerate with: ORCHESTRATOR_ACCEPTED_GO_BIN=<go binary> \
             crates/orchestrator-cli/tests/fixtures/learning/regenerate.sh",
            path.display()
        )
    })?)
}

/// The local flags Go registers for each command, read out of its own `--help`.
///
/// Global flags are excluded: they belong to the root command and are already
/// covered by `go_cli_differential`.
fn go_local_flags(command: &str) -> TestResult<Vec<String>> {
    let help = go_help(command)?;
    let local = help
        .split("Global Flags:")
        .next()
        .unwrap_or(&help)
        .split("Flags:")
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    Ok(local
        .lines()
        .filter_map(|line| {
            line.split_whitespace()
                .find(|token| token.starts_with("--"))
                .map(|token| token.trim_end_matches(',').to_owned())
        })
        .filter(|flag| flag != "--help")
        .collect())
}

#[test]
fn every_go_local_flag_is_accepted_by_the_port() -> TestResult {
    // A flag Go registers but this port rejects would be a silent surface gap.
    // Each flag is offered with a value that is valid for its type; a rejection
    // surfaces as an "unknown flag" parse error rather than the port gap.
    let fixture = Fixture::create()?;
    let reader = fixture.reader()?;
    let values: &[(&str, &str)] = &[
        ("--apply", "--apply"),
        ("--max-age", "--max-age=1"),
        ("--min-score", "--min-score=0.5"),
        ("--max-count", "--max-count=1"),
        ("--domain", "--domain=dev"),
        ("--db", "--db=/dev/null"),
        ("--since", "--since=1h"),
        ("--limit", "--limit=1"),
        ("--batch-size", "--batch-size=1"),
        ("--rpm", "--rpm=1"),
        ("--max-retries", "--max-retries=1"),
        ("--include-archived", "--include-archived"),
        ("--quiet", "--quiet"),
    ];

    for command in ["stats", "prune", "archive", "backfill-embeddings"] {
        let flags = go_local_flags(command)?;
        assert!(
            !flags.is_empty() || command == "stats",
            "no local flags parsed out of Go's {command} help; the capture format changed"
        );
        for flag in flags {
            let (_, spelled) = values
                .iter()
                .find(|(name, _)| *name == flag)
                .ok_or(format!(
                    "Go registers {flag} for {command}; add it to the port"
                ))?;
            // `--apply` is still refused, so an error is allowed here; what
            // is not allowed is a *parse* rejection of a flag Go registers.
            if let (_, Err(message)) = invoke(&reader, &[command, spelled]) {
                assert!(
                    !message.contains("unknown flag"),
                    "{command} rejects Go's {flag}: {message}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn an_unregistered_flag_is_rejected() -> TestResult {
    let fixture = Fixture::create()?;
    let reader = fixture.reader()?;
    for command in ["stats", "prune", "archive", "backfill-embeddings"] {
        let (_, result) = invoke(&reader, &[command, "--definitely-not-a-flag"]);
        let message = result
            .err()
            .ok_or(format!("{command} accepted a bogus flag"))?;
        assert!(
            message.contains("unknown flag"),
            "{command} failed for the wrong reason: {message}"
        );
    }
    Ok(())
}

#[test]
fn go_rejects_the_same_unregistered_flag() -> TestResult {
    // Confirms the previous test is testing a real rule and not a Rust-only
    // invention. A missing oracle is a hard failure naming exactly what was
    // checked, never an `eprintln!` + `Ok(())` green (TRK-1280) — the same rule
    // `go_cli_differential.rs` enforces.
    let go_binary = accepted_go_binary()?;
    let fixture = Fixture::create()?;
    for command in ["stats", "prune", "archive", "backfill-embeddings"] {
        let output = Command::new(&go_binary)
            .args([command, "--definitely-not-a-flag"])
            .env_clear()
            .env("HOME", fixture.root.path())
            .env("ORCHESTRATOR_CONFIG_DIR", fixture.root.path())
            .env("PATH", "")
            .output()?;
        assert_ne!(
            output.status.code(),
            Some(0),
            "Go accepted --definitely-not-a-flag for {command}"
        );
    }
    Ok(())
}

/// The accepted Go orchestrator binary the live cross-check runs against.
/// This must never resolve to a silent skip: a missing oracle is a hard
/// failure that names exactly what was checked and where, not an
/// `eprintln!` + `Ok(())` green (TRK-1280).
fn accepted_go_binary() -> Result<PathBuf, String> {
    // B5-DESIGN §8.6 rung 1: inside the verification lease the oracle is the
    // one the lease built from the tested commit, and nothing else is
    // consulted — not `ORCHESTRATOR_ACCEPTED_GO_BIN`, and above all not
    // `$HOME/.alluka/bin/orchestrator`, which is the installed binary TRK-1280
    // showed cannot witness a claim about a committed tree.
    if let Some(directory) = std::env::var_os("NANIKA_GO_ORACLE_OUTPUT_DIR") {
        let leased = PathBuf::from(directory).join("orchestrator");
        return if leased.is_file() {
            Ok(leased)
        } else {
            Err(format!(
                "NANIKA_GO_ORACLE_OUTPUT_DIR names no built Go oracle at {}; the verification \
                 lease did not produce one",
                leased.display()
            ))
        };
    }
    if let Some(path) = std::env::var_os("ORCHESTRATOR_ACCEPTED_GO_BIN") {
        let path = PathBuf::from(path);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!(
                "ORCHESTRATOR_ACCEPTED_GO_BIN={} does not name a file; the accepted Go oracle binary is missing",
                path.display()
            ))
        };
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| {
            "HOME is unset and ORCHESTRATOR_ACCEPTED_GO_BIN is not set; cannot locate the accepted Go oracle binary".to_string()
        })?;
    let default_path = home.join(".alluka/bin/orchestrator");
    if default_path.is_file() {
        Ok(default_path)
    } else {
        Err(format!(
            "the accepted Go oracle binary is missing: neither ORCHESTRATOR_ACCEPTED_GO_BIN nor the default {} names an existing file",
            default_path.display()
        ))
    }
}

// ---------------------------------------------------------------------------
// 6. Byte parity with the frozen Go dry-run captures (TRK-1248)
// ---------------------------------------------------------------------------

#[test]
fn every_frozen_go_dry_run_capture_is_reproduced_byte_for_byte() -> TestResult {
    // B3-DESIGN's Gate 4 asks for byte parity on the dry-run data lines. This
    // replays each frozen capture against the same store Go was pointed at and
    // compares stdout bytes and exit status. `argv` comes out of the capture
    // directory, so a case added to the oracle is covered without editing this
    // file, and a case cannot be silently replayed with different arguments.
    let cases = Oracle::all_command_cases()?;
    for expected in [
        "stats",
        "prune-dry",
        "prune-dry-flags",
        "archive-dry",
        "archive-dry-domain",
        "backfill-dry",
        "backfill-dry-limited",
    ] {
        assert!(
            cases.iter().any(|case| case.name == expected),
            "the {expected} capture disappeared from the oracle"
        );
    }

    let fixture = Fixture::create()?;
    let reader = fixture.reader()?;
    for case in &cases {
        assert!(
            !case.stdout.is_empty(),
            "the {} oracle is empty; regenerate the fixture",
            case.name
        );
        let argv: Vec<&str> = case.argv.iter().map(String::as_str).collect();
        let (rendered, result) = invoke(&reader, &argv);
        let status = match &result {
            Ok(()) => 0,
            Err(message) => {
                return Err(format!("{} ({argv:?}) failed: {message}", case.name).into());
            }
        };
        assert_eq!(status, case.exit, "{} exit status", case.name);
        assert_eq!(
            String::from_utf8(rendered)?,
            case.stdout,
            "{} stdout diverged from the frozen Go capture",
            case.name
        );
    }
    Ok(())
}

#[test]
fn the_oracle_still_carries_the_strings_gate_4_names() -> TestResult {
    // Guards the comparison above against a fixture that quietly loses the
    // exact wording B3-DESIGN §Gate 4 calls out: byte equality against a
    // drifted oracle would still pass.
    assert!(
        Oracle::load("prune-dry")?
            .stdout
            .contains("(use --apply to delete)"),
        "the prune dry-run oracle lost its declared string"
    );
    assert!(
        Oracle::load("archive-dry")?
            .stdout
            .contains("(use --apply to write)"),
        "the archive dry-run oracle lost its declared string"
    );
    Ok(())
}

#[test]
fn a_missing_oracle_is_a_hard_failure_rather_than_a_skip() {
    assert!(
        Oracle::load("no-such-case").is_err(),
        "a missing oracle capture did not fail the gate"
    );
    assert!(
        Oracle::load("prune-dry").is_ok(),
        "the loader rejects a capture that exists"
    );
}
