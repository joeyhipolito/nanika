#![cfg(unix)]
//! B5-DESIGN §3 and §4 Gate 2 — the Go↔Rust upgrade/rollback ladder.
//!
//! `ORCHESTRATOR_ACCEPTED_GO_BIN=<abs path> cargo test -p orchestrator-cli
//! --locked --offline --test go_rust_upgrade_rollback_e2e -- --nocapture`
//!
//! Six rungs (§3.4), each a `#[test]` over its own copied home:
//!
//! | Rung | Actor | Action |
//! |---|---|---|
//! | R0 | Go   | read the seeded home (`status`, `events list`, `events replay`) |
//! | R1 | Rust | write: run one fixture-backed mission to terminal |
//! | R2 | Rust | **crash**: `SIGKILL` at the metrics-commit-before-acknowledgement window |
//! | R3 | Rust | restart, recover, complete |
//! | R4 | Go   | read the Rust-written home, then **continue** it to a second terminal |
//! | R5 | Go   | read again after the continuation |
//!
//! ## What makes this a rollback test rather than a compatibility test
//!
//! Production binary selection is a symlink, and `scripts/atomic-activate-symlink.py`
//! already exists, so the gate reuses it rather than inventing a mechanism
//! (§3.5). Every leg — Go's and Rust's alike — is spawned as
//! `<prefix>/bin/orchestrator-rs`, the activation link, never as an absolute
//! path to a binary. R0 runs with Go selected, the gate then installs and
//! activates the Rust sidecar for R1–R3, and R4 flips **back** to the packaged
//! Go binary and runs through the same link. What is proven is the selection
//! mechanism, not just the on-disk format.
//!
//! The activation helper fixes the link name (`ACTIVE_NAME = "orchestrator-rs"`)
//! and requires the target to be `<versions>/<64 hex>/orchestrator-rs`, so the
//! packaged Go binary is installed under that name too. The link name is the
//! *selector*; which implementation is selected is the link's target, and that
//! is what [`Prefix::selected`] reads back and what the rollback assertions
//! compare by digest.
//!
//! **Go remains the default throughout.** The gate installs Rust, flips to it,
//! and flips back; no test leaves the Rust sidecar selected, and
//! [`the_gate_leaves_the_packaged_go_binary_selected`] asserts that directly.
//!
//! ## The Rust sidecar
//!
//! The shipped `orchestrator` binary passes `None` to `composition::seal` and
//! therefore refuses to execute (`CliError::ExecutionNotEnrolled`) — that is
//! B5-DESIGN §2.2 and `run_not_enrolled.rs` owns it. The enrolled branch exists
//! only in a `test-support` build, so the "Rust bundle" this gate installs and
//! activates is *this test binary*, re-exec'd into a writer mode. It is a thin
//! in-tree build artifact, it is also the attested helper its own runs install,
//! and it reaches the enrolled path through the same
//! `composition::seal(&flags, Some(&enrollment))` call the other B5 gates use.
//!
//! ## Copied home, never the live one (§3.2)
//!
//! Every leg runs against a home copied into an `IsolatedFixtureRoot` from
//! `tests/fixtures/upgrade-rollback/` **by explicit manifest**, so the
//! pre-state is itself a comparable artifact:
//! [`the_seed_manifest_is_exactly_the_seed_tree`] fails if the manifest and the
//! tree disagree in either direction. `NANIKA_LIVE_HOME_ENROLL` is unset in
//! every leg and appears nowhere in the allowlist
//! ([`LEG_ENVIRONMENT`]), so the gate proves rollback on the fixture-home
//! authorization branch, which is the only branch B5 enrolls.
//!
//! ## The oracle is in-tree and mandatory (§3.1)
//!
//! [`frozen_go_oracle`] builds the in-tree Go orchestrator through
//! `tests/build-go-oracle.sh` and accepts `ORCHESTRATOR_ACCEPTED_GO_BIN` only
//! when its sha256 equals that build's. A missing, unbuildable or mismatched
//! oracle is a hard failure naming what was checked and where — never an
//! `eprintln!` + `Ok(())` green. An installed binary was once built from
//! uncommitted `run.go` (TRK-1280) and cannot witness a claim about a committed
//! tree.
//!
//! ## What the ladder found, and what it therefore claims
//!
//! Two findings are **recorded, not smoothed over**, and each is asserted in
//! both directions so it cannot decay into a test that did not run:
//!
//! 1. **Go's `status` crashes on a plan-less checkpoint — and Rust no longer
//!    writes one.** `internal/cmd/status.go:63` dereferences `cp.Plan` with no
//!    nil check, so the frozen oracle exits 2 with a nil-pointer `SIGSEGV` on
//!    any checkpoint whose plan is absent. That defect is the owner's
//!    (TRK-1291) and is untouched here. What CF-M4a-4 changed is the input:
//!    the composition root seeds the empty placeholder plan and
//!    `SealedRun::execute` publishes the authored phases into it before the
//!    first dispatch, so rollback no longer lands Go on a checkpoint it cannot
//!    read.
//!    [`a_rust_authored_checkpoint_carries_a_plan_the_rolled_back_go_binary_reads`]
//!    pins all three halves — Go's defect is real and still there, a completed
//!    Rust run leaves a plan carrying every released phase, and the rolled-back
//!    binary's `status` exits 0 over that home and counts those phases.
//! 2. **A restart re-executes rather than resumes.** The enrolled `execute`
//!    runs the resolved plan, so the canonical log after R3 carries a second
//!    attempt and the mission row's aggregate counters — which
//!    `record_terminal` recomputes as `count(*)` over the phase rows present at
//!    that instant — shift by one. What converges is the phase row, keyed on
//!    mission+phase and upserted. R3 pins the doubled event sequence, the
//!    identical phase row, and the exact two mission-row fields that move.
//!
//! ## Timing-barrier protocol
//!
//! R2 forks and `SIGKILL`s a subprocess, so per §4 this gate records the load
//! average: [`load_average_is_recorded_for_this_gate`] writes it to a fixed
//! file and fails if it could not. A crash gate that passes without a recorded
//! load average is not evidence.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};

use orchestrator_app::{
    FixtureAdmissionPolicy, FreshFixtureAuthority, IsolatedFixtureRoot, PhaseMetricIntent,
};
use orchestrator_cli::{FixtureEnrollment, PersistentFlags, RunFlags, resolve_with_context, seal};
use orchestrator_core::{MissionId, PhaseId};

mod support;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static CASE: AtomicU64 = AtomicU64::new(1);

const FIXTURE_RUNTIME: &str = "codex";
const HELPER: &str = "attested-helper";
const HELPER_QUICK: &[&str] = &["--exact", "helper_quick_entrypoint", "--nocapture"];

const SIDECAR_MODE: &str = "NANIKA_B5_ROLLBACK_MODE";
const SIDECAR_ROOT: &str = "NANIKA_B5_ROLLBACK_ROOT";
const SIDECAR_PARENT: &str = "NANIKA_B5_ROLLBACK_PARENT";
const SIDECAR_MISSION: &str = "NANIKA_B5_ROLLBACK_MISSION";
const SIDECAR_MARKER: &str = "NANIKA_B5_ROLLBACK_MARKER";

/// The seeded pre-state's mission, and the mission the Rust sidecar writes.
const SEED_MISSION: &str = "seed-mission";
const RUST_MISSION: &str = "rust-written";

/// The activation link's fixed name, from `atomic-activate-symlink.py`.
const ACTIVE_NAME: &str = "orchestrator-rs";

/// How long a parked sidecar waits to be killed before giving up.
const PARK_LIMIT: Duration = Duration::from_secs(120);
const BARRIER_DEADLINE: Duration = Duration::from_secs(60);

/// The complete environment any leg is given (§3.3, modelled on
/// `tests/writer-authority-cross-language.sh`: "accepts no ambient toolchain,
/// Cargo, Go, Git, wrapper, runner, or configuration input").
///
/// `NANIKA_LIVE_HOME_ENROLL` is deliberately absent, and
/// [`the_leg_allowlist_never_carries_live_home_enrollment`] asserts it.
const LEG_ENVIRONMENT: [&str; 5] = [
    "HOME",
    "ORCHESTRATOR_CONFIG_DIR",
    "ORCHESTRATOR_PERSONAS_DIR",
    "PATH",
    "TMPDIR",
];

/// The seed manifest and the files it names, embedded so the gate carries its
/// own pre-state rather than reading whatever happens to be on disk.
const SEED_MANIFEST: &str = include_str!("fixtures/upgrade-rollback/manifest");

fn seed_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upgrade-rollback")
}

// ---------------------------------------------------------------------------
// The oracle (B5-DESIGN §3.1)
// ---------------------------------------------------------------------------

/// Builds the in-tree Go orchestrator and returns the accepted binary.
fn frozen_go_oracle() -> Result<PathBuf, String> {
    let manifest = support::frozen_tree_manifest();
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
    // The builder prints the oracle it resolved: under the verification lease
    // that is the lease-owned output directory (B5-DESIGN §8.6 rung 1) and in a
    // bare run it is `target/go-oracle/orchestrator`. Reading it back rather
    // than hardcoding the bare path keeps one definition of where the oracle
    // lives.
    let built = resolved_oracle_path(&output.stdout, &builder)?;
    let reference = sha256_file(&built).map_err(|error| {
        format!(
            "the in-tree Go oracle build produced no readable binary at {}: {error}",
            built.display()
        )
    })?;
    // One resolver, not two: `support::frozen_go_oracle` is the definition the
    // other four gates in this crate share — it honours the lease's
    // `NANIKA_GO_ORACLE_OUTPUT_DIR`, then `ORCHESTRATOR_ACCEPTED_GO_BIN`, then
    // the workspace's own `target/go-oracle/orchestrator`, and hard-fails
    // naming the manifest. This gate layers §3.1's digest rule on top of it
    // rather than restating it, so the two cannot drift.
    let candidate = support::frozen_go_oracle()?;
    accept_cached_oracle(&candidate, &built, &reference, &manifest)
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

/// §3.1's rule about the cache, as a pure function so it can be asserted
/// directly rather than by mutating the environment of a test that runs beside
/// the ladder.
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

/// The interpreter `atomic-activate-symlink.py` declares in its shebang. This
/// gate already depends on it for activation, so it is also what computes the
/// version identifiers — no new Rust dependency, and no `Cargo.lock` movement.
fn python3() -> PathBuf {
    PathBuf::from("/usr/bin/python3")
}

fn sha256_file(path: &Path) -> Result<String, String> {
    if !path.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    let interpreter = python3();
    if !interpreter.is_file() {
        return Err(format!(
            "{} is missing; it is the interpreter atomic-activate-symlink.py declares",
            interpreter.display()
        ));
    }
    let output = Command::new(interpreter)
        .arg("-c")
        .arg("import hashlib,sys;print(hashlib.sha256(open(sys.argv[1],'rb').read()).hexdigest())")
        .arg(path)
        .output()
        .map_err(|error| format!("digesting {} failed: {error}", path.display()))?;
    if !output.status.success() {
        return Err(format!(
            "digesting {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

// ---------------------------------------------------------------------------
// Observations, and the named masking rules (§3.4 comparison class 1)
// ---------------------------------------------------------------------------

/// One leg's exact result: stdout and stderr as bytes, the exit code, and —
/// where the leg is expected to die — the signal.
#[derive(Debug, Eq, PartialEq)]
struct Observation {
    exit_code: Option<i32>,
    signal: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Observation {
    fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    /// Applies every masking rule, in a fixed order, to both streams.
    ///
    /// Each volatile field has its own named rule; there is no regex sweep, so
    /// a field that starts varying is a failure until someone writes a rule for
    /// it and says why.
    fn masked(&self, roots: &[(&Path, &str)]) -> (String, String) {
        (
            mask(&self.stdout_text(), roots),
            mask(&self.stderr_text(), roots),
        )
    }
}

fn mask(text: &str, roots: &[(&Path, &str)]) -> String {
    let mut masked = mask_absolute_roots(text, roots);
    masked = mask_rfc3339_timestamps(&masked);
    masked = mask_durations(&masked);
    masked = mask_event_identifiers(&masked);
    masked
}

/// Rule 1 — absolute roots. Every leg runs under a per-test temporary root
/// whose name carries a pid and a counter, so the path is volatile by
/// construction and is replaced by its declared label.
fn mask_absolute_roots(text: &str, roots: &[(&Path, &str)]) -> String {
    let mut masked = text.to_owned();
    for (path, label) in roots {
        masked = masked.replace(&path.to_string_lossy().into_owned(), label);
    }
    masked
}

/// Rule 2 — RFC3339 timestamps, `2026-09-03T02:37:51.605672Z` and friends.
fn mask_rfc3339_timestamps(text: &str) -> String {
    let bytes: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        if let Some(width) = rfc3339_width(&bytes[index..]) {
            out.push_str("<timestamp>");
            index += width;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    out
}

/// `YYYY-MM-DDTHH:MM:SS` optionally followed by `.<fraction>` and `Z`.
fn rfc3339_width(window: &[char]) -> Option<usize> {
    const SHAPE: &str = "dddd-dd-ddTdd:dd:dd";
    if window.len() < SHAPE.len() {
        return None;
    }
    for (offset, expected) in SHAPE.chars().enumerate() {
        let observed = window[offset];
        let matched = match expected {
            'd' => observed.is_ascii_digit(),
            other => observed == other,
        };
        if !matched {
            return None;
        }
    }
    let mut width = SHAPE.len();
    if window.get(width) == Some(&'.') {
        width += 1;
        while window.get(width).is_some_and(char::is_ascii_digit) {
            width += 1;
        }
    }
    if window.get(width) == Some(&'Z') {
        width += 1;
    }
    Some(width)
}

/// Rule 3 — Go `time.Duration` renderings, `25.594667ms`, `0s`, `1.2µs`.
fn mask_durations(text: &str) -> String {
    const UNITS: [&str; 5] = ["ns", "µs", "ms", "s", "m"];
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index].is_ascii_digit()
            && (index == 0 || !chars[index - 1].is_ascii_alphanumeric())
        {
            let mut end = index;
            while chars.get(end).is_some_and(char::is_ascii_digit) {
                end += 1;
            }
            if chars.get(end) == Some(&'.') {
                let mut fraction = end + 1;
                while chars.get(fraction).is_some_and(char::is_ascii_digit) {
                    fraction += 1;
                }
                if fraction > end + 1 {
                    end = fraction;
                }
            }
            let tail: String = chars[end..].iter().take(2).collect();
            if let Some(unit) = UNITS.iter().find(|unit| tail.starts_with(**unit)) {
                let after = end + unit.chars().count();
                let boundary = chars
                    .get(after)
                    .is_none_or(|next| !next.is_ascii_alphanumeric());
                if boundary {
                    out.push_str("<duration>");
                    index = after;
                    continue;
                }
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

/// Rule 4 — canonical event identifiers, `evt_` plus 16 hex digits.
fn mask_event_identifiers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let window: String = chars[index..].iter().take(4).collect();
        if window == "evt_" {
            let digits = chars[index + 4..]
                .iter()
                .take_while(|value| value.is_ascii_hexdigit())
                .count();
            if digits == 16 {
                out.push_str("<event-id>");
                index += 4 + digits;
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Rows and events (§3.4 comparison classes 2 and 3)
// ---------------------------------------------------------------------------

/// An ordered dump of one SQLite home artifact.
///
/// Rows are rendered `table|column=value|…` and sorted, so the comparison is
/// over an ordered row *set*: a row that changes, moves between tables, or
/// disappears fails. Row count equality alone is never accepted, which
/// [`a_single_cell_mutation_fails_the_row_comparison`] demonstrates directly.
///
/// The connection is read-*write* on purpose: the Go orchestrator leaves its
/// SQLite stores in WAL mode and removes `-wal`/`-shm` on exit, so a
/// `SQLITE_OPEN_READ_ONLY` connection cannot create the shared-memory index and
/// fails on the first `prepare`. Opening read-write changes no row.
fn sqlite_dump(path: &Path) -> TestResult<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let tables: Vec<String> = {
        let mut statement = connection.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        let mut names = Vec::new();
        let mut cursor = statement.query([])?;
        while let Some(row) = cursor.next()? {
            names.push(row.get::<_, String>(0)?);
        }
        names
    };
    let mut rows = Vec::new();
    for table in tables {
        let mut statement = connection.prepare(&format!("SELECT * FROM \"{table}\""))?;
        let columns: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut cursor = statement.query([])?;
        while let Some(row) = cursor.next()? {
            let mut rendered = String::from(&table);
            for (index, column) in columns.iter().enumerate() {
                rendered.push('|');
                rendered.push_str(column);
                rendered.push('=');
                rendered.push_str(&cell(row, index, column)?);
            }
            rows.push(rendered);
        }
    }
    rows.sort();
    Ok(rows)
}

/// Renders one cell, masking the columns whose values are volatile by nature.
///
/// Named rules again, by column: anything ending `_at` is a wall-clock stamp
/// and anything named for a duration is a measured elapsed time. Nothing else
/// is masked, so a value that starts varying fails until a rule is written.
fn cell(row: &rusqlite::Row<'_>, index: usize, column: &str) -> TestResult<String> {
    if column.ends_with("_at") || column == "duration_s" || column == "duration" {
        return Ok("<volatile>".to_owned());
    }
    let value = row.get_ref(index)?;
    Ok(match value {
        rusqlite::types::ValueRef::Null => "null".to_owned(),
        rusqlite::types::ValueRef::Integer(number) => number.to_string(),
        rusqlite::types::ValueRef::Real(number) => format!("{number:?}"),
        rusqlite::types::ValueRef::Text(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        rusqlite::types::ValueRef::Blob(bytes) => format!("blob:{}", bytes.len()),
    })
}

/// The dumped rows belonging to one table, in the dump's order.
fn rows_named<'a>(table: &str, rows: &'a [String]) -> Vec<&'a String> {
    let prefix = format!("{table}|");
    rows.iter().filter(|row| row.starts_with(&prefix)).collect()
}

/// The `name=value` field names on which two dumped rows disagree, sorted.
///
/// A field-level diff rather than a whole-row inequality, so a recorded
/// divergence names exactly which cells moved and nothing more.
fn differing_fields(left: &str, right: &str) -> Vec<String> {
    let split = |row: &str| -> Vec<(String, String)> {
        row.split('|')
            .skip(1)
            .filter_map(|field| {
                field
                    .split_once('=')
                    .map(|(name, value)| (name.to_owned(), value.to_owned()))
            })
            .collect()
    };
    let right_fields = split(right);
    let mut names: Vec<String> = split(left)
        .into_iter()
        .zip(right_fields)
        .filter(|((_, left_value), (_, right_value))| left_value != right_value)
        .map(|((name, _), _)| name)
        .collect();
    names.sort();
    names
}

/// The canonical event projection for one mission, as an ordered sequence of
/// typed facts rather than raw file bytes — the log is append-ordered but its
/// framing carries offsets (§3.4 comparison class 3).
fn event_facts(root: &Path, mission: &str) -> Vec<(String, String)> {
    let path = root.join("events").join(format!("{mission}.jsonl"));
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

fn string_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

// ---------------------------------------------------------------------------
// The production selection prefix (§3.5)
// ---------------------------------------------------------------------------

/// A production-shaped install prefix: `bin/` holds the activation link and
/// `libexec/nanika-orchestrator/<id>/orchestrator-rs` holds each sealed
/// version, exactly the layout `install-development-bundle.sh` publishes and
/// `atomic-activate-symlink.py` validates.
struct Prefix {
    bin: PathBuf,
    versions: PathBuf,
    installed: Vec<PathBuf>,
}

impl Prefix {
    fn create(parent: &Path) -> TestResult<Self> {
        let prefix = parent.join("prefix");
        let bin = prefix.join("bin");
        let libexec = prefix.join("libexec");
        let versions = libexec.join("nanika-orchestrator");
        for directory in [&prefix, &bin, &libexec, &versions] {
            std::fs::create_dir_all(directory)?;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            bin,
            versions,
            installed: Vec::new(),
        })
    }

    /// Installs one binary as a sealed version and returns its target path.
    ///
    /// The identifier is the binary's sha256, which is what makes the
    /// rollback assertions checkable: the selected target's identifier *is*
    /// the digest of the implementation selected.
    fn install(&mut self, binary: &Path) -> TestResult<PathBuf> {
        let identifier = sha256_file(binary)?;
        let directory = self.versions.join(&identifier);
        if !directory.is_dir() {
            std::fs::create_dir_all(&directory)?;
            let target = directory.join(ACTIVE_NAME);
            std::fs::copy(binary, &target)?;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o500))?;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500))?;
            self.installed.push(directory.clone());
        }
        Ok(directory.join(ACTIVE_NAME))
    }

    /// Flips the activation link, through the shipped helper.
    fn activate(&self, target: &Path) -> TestResult {
        let helper = workspace_root().join("scripts/atomic-activate-symlink.py");
        if !helper.is_file() {
            return Err(format!("the activation helper {} is missing", helper.display()).into());
        }
        let output = Command::new(python3())
            .arg(&helper)
            .arg(&self.bin)
            .arg(&self.versions)
            .arg(target)
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "activation of {} failed with {:?}: {}",
                target.display(),
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .into());
        }
        Ok(())
    }

    /// The path every leg is spawned as.
    fn selector(&self) -> PathBuf {
        self.bin.join(ACTIVE_NAME)
    }

    /// The link's current target, read back without following it.
    fn selected(&self) -> TestResult<PathBuf> {
        Ok(std::fs::read_link(self.selector())?)
    }
}

impl Drop for Prefix {
    fn drop(&mut self) {
        // The sealed version directories are mode 0500, which is what the
        // activation helper requires and what stops `remove_dir_all`.
        for directory in &self.installed {
            let _ = std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700));
        }
    }
}

// ---------------------------------------------------------------------------
// The ladder's home
// ---------------------------------------------------------------------------

/// One private, seeded home plus its selection prefix.
///
/// The parent creates the fixture root, admits it once so it carries the v2
/// marker `FreshFixtureAuthority::recover` requires, and then *lets go* of the
/// authority, because the authority holds an exclusive `flock` and every leg
/// after this is a separate process.
struct Ladder {
    parent: PathBuf,
    root: PathBuf,
    prefix: Prefix,
    go: PathBuf,
    rust: PathBuf,
    go_target: PathBuf,
    rust_target: PathBuf,
    /// Every leg's `wait()` result, recorded in spawn order. §4's "no
    /// concurrent access" assertion reads this.
    reaped: Vec<String>,
}

impl Drop for Ladder {
    fn drop(&mut self) {
        for directory in &self.prefix.installed {
            let _ = std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700));
        }
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

impl Ladder {
    fn new(label: &str) -> TestResult<Self> {
        let go = frozen_go_oracle()?;
        let rust = std::env::current_exe()?;
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let temporary = std::fs::canonicalize(std::env::temp_dir())?;
        let parent = temporary.join(format!(
            "orchestrator-rs-b5-rollback-{}-{number}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&parent);
        private_dir(&parent)?;
        for leaf in ["go-user", "empty-path"] {
            private_dir(&parent.join(leaf))?;
        }

        let bytes = std::fs::read(&rust)?;
        let root = {
            let fresh = IsolatedFixtureRoot::create_fresh(&parent)?;
            let path = fresh.path().to_path_buf();
            let authority = FreshFixtureAuthority::admit(fresh, &policy(&parent, &bytes)?)?;
            drop(authority);
            path
        };
        copy_seed(&root, &parent.join("personas"))?;

        let mut prefix = Prefix::create(&parent)?;
        let go_target = prefix.install(&go)?;
        let rust_target = prefix.install(&rust)?;
        // Go is selected first and is the default throughout (§3.5).
        prefix.activate(&go_target)?;

        Ok(Self {
            parent,
            root,
            prefix,
            go,
            rust,
            go_target,
            rust_target,
            reaped: Vec::new(),
        })
    }

    fn personas(&self) -> PathBuf {
        self.parent.join("personas")
    }

    /// Spawns one leg **through the activation link** and reaps it before
    /// returning, so no two legs are ever live against the same home.
    fn leg(
        &mut self,
        name: &str,
        argv: &[&str],
        extra: &[(&str, &str)],
    ) -> TestResult<Observation> {
        let mut command = Command::new(self.prefix.selector());
        command
            .args(argv)
            .current_dir(&self.parent)
            .env_clear()
            .env("HOME", self.parent.join("go-user"))
            .env("ORCHESTRATOR_CONFIG_DIR", &self.root)
            .env("ORCHESTRATOR_PERSONAS_DIR", self.personas())
            .env("PATH", self.parent.join("empty-path"))
            .env("TMPDIR", &self.parent)
            .stdin(Stdio::null());
        for (key, value) in extra {
            command.env(key, value);
        }
        let output = command.output()?;
        self.reaped.push(format!(
            "{name}: exit={:?} signal={:?}",
            output.status.code(),
            output.status.signal()
        ));
        Ok(Observation {
            exit_code: output.status.code(),
            signal: output.status.signal(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn sidecar(&mut self, name: &str, mode: &str, mission: &str) -> TestResult<Observation> {
        let root = self.root.to_string_lossy().into_owned();
        let parent = self.parent.to_string_lossy().into_owned();
        self.leg(
            name,
            HELPER_QUICK_SIDECAR,
            &[
                (SIDECAR_MODE, mode),
                (SIDECAR_ROOT, &root),
                (SIDECAR_PARENT, &parent),
                (SIDECAR_MISSION, mission),
            ],
        )
    }

    fn masked(&self, observation: &Observation) -> (String, String) {
        observation.masked(&[(&self.root, "<root>"), (&self.parent, "<parent>")])
    }

    fn metrics_rows(&self) -> TestResult<Vec<String>> {
        sqlite_dump(&self.root.join("metrics.db"))
    }

    fn runtime_rows(&self) -> TestResult<Vec<String>> {
        sqlite_dump(&self.root.join("runtime.db"))
    }

    fn events(&self, mission: &str) -> Vec<(String, String)> {
        event_facts(&self.root, mission)
    }

    /// Flips the selector to the packaged Go binary — the rollback (§3.5).
    fn roll_back_to_go(&self) -> TestResult {
        self.prefix.activate(&self.go_target)
    }

    /// Flips the selector to the Rust sidecar — the upgrade.
    fn upgrade_to_rust(&self) -> TestResult {
        self.prefix.activate(&self.rust_target)
    }

    /// Asserts the selector currently resolves to the given implementation, by
    /// digest, and that the leg reached it through the link rather than a path.
    fn assert_selected(&self, binary: &Path, which: &str) -> TestResult {
        let target = self.prefix.selected()?;
        let expected_digest = sha256_file(binary)?;
        let selected_digest = sha256_file(&target)?;
        assert_eq!(
            selected_digest,
            expected_digest,
            "the activation link points at {} rather than the {which} binary",
            target.display()
        );
        let resolved = std::fs::canonicalize(self.prefix.selector())?;
        assert_eq!(
            resolved,
            std::fs::canonicalize(&target)?,
            "the selector does not resolve through the activation link",
        );
        assert!(
            resolved.starts_with(&self.prefix.versions),
            "the selector resolves outside the versions directory: {}",
            resolved.display()
        );
        Ok(())
    }
}

/// The argv the installed Rust sidecar is spawned with. `libtest` filters to
/// the one entry point, so the sidecar's mode variables cannot leak into any
/// other test in this binary — including the attested helper, which runs
/// `helper_quick_entrypoint` instead.
const HELPER_QUICK_SIDECAR: &[&str] = &["--exact", "sidecar_entrypoint", "--nocapture"];

fn policy(parent: &Path, helper: &[u8]) -> TestResult<FixtureAdmissionPolicy> {
    let temporary = std::fs::canonicalize(std::env::temp_dir())?;
    let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
    Ok(
        FixtureAdmissionPolicy::new(parent.join("live-user"), checkout, &temporary)
            .with_expected_fixture_helper(helper),
    )
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

/// Creates every component of `directory` beneath `base` with mode 0700.
fn private_tree(base: &Path, directory: &Path) -> TestResult {
    let relative = directory.strip_prefix(base)?;
    let mut current = base.to_path_buf();
    for component in relative.components() {
        current = current.join(component);
        private_dir(&current)?;
    }
    Ok(())
}

/// Copies the seeded pre-state by explicit manifest (§3.2).
///
/// A manifest line that names a missing file is an error, so the copy cannot
/// silently produce a thinner home than the one the gate claims to seed.
fn copy_seed(root: &Path, personas: &Path) -> TestResult {
    private_dir(personas)?;
    for relative in seed_entries() {
        let source = seed_root().join(&relative);
        if !source.is_file() {
            return Err(format!(
                "the seed manifest names {}, which is not a file",
                source.display()
            )
            .into());
        }
        let (base, destination) = match relative.strip_prefix("home/") {
            Some(tail) => (root, root.join(tail)),
            None => match relative.strip_prefix("personas/") {
                Some(tail) => (personas, personas.join(tail)),
                None => {
                    return Err(format!(
                        "seed manifest entry {relative:?} names neither home/ nor personas/"
                    )
                    .into());
                }
            },
        };
        if let Some(directory) = destination.parent() {
            // Every component, not just the leaf: `ensure_private_child`
            // refuses a group- or world-readable directory beneath the
            // boundary, and `create_dir_all` would leave the intermediate
            // `workspaces/` at the process umask.
            private_tree(base, directory)?;
        }
        std::fs::copy(&source, &destination)?;
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn seed_entries() -> Vec<String> {
    SEED_MANIFEST
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

// ---------------------------------------------------------------------------
// The Rust sidecar's re-exec entry point
// ---------------------------------------------------------------------------

/// Inert unless [`SIDECAR_MODE`] is set, which only [`Ladder::sidecar`] does.
#[test]
fn sidecar_entrypoint() {
    let Ok(mode) = std::env::var(SIDECAR_MODE) else {
        return;
    };
    match run_sidecar(&mode) {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("sidecar {mode} failed: {error}");
            std::process::exit(2);
        }
    }
}

fn run_sidecar(mode: &str) -> TestResult {
    let root_path = PathBuf::from(std::env::var(SIDECAR_ROOT)?);
    let parent = PathBuf::from(std::env::var(SIDECAR_PARENT)?);
    let mission = MissionId::new(std::env::var(SIDECAR_MISSION)?)?;
    // The sidecar's own bytes are the attested helper's bytes: it is the
    // installed version's file, so `current_exe` is the sealed copy under
    // `<versions>/<id>/orchestrator-rs`, not the build tree's.
    let bytes = std::fs::read(std::env::current_exe()?)?;
    let policy = policy(&parent, &bytes)?;

    let root = IsolatedFixtureRoot::identify(&root_path)?;
    let authority =
        FreshFixtureAuthority::recover(IsolatedFixtureRoot::identify(&root_path)?, &policy)?;
    let enrollment = FixtureEnrollment {
        root: &root,
        authority: &authority,
        helper_label: HELPER,
        helper_bytes: &bytes,
        helper_arguments: HELPER_QUICK,
        mission: mission.clone(),
        phase: PhaseId::new("phase-1")?,
        runtime: FIXTURE_RUNTIME,
        git: None,
        knowledge: None,
        evidence: None,
    };
    let mut sealed = seal(&ordinary_flags(), Some(&enrollment))?;
    let resolved = resolve_with_context(&[mission_source()], sealed.resolution())?;
    let report = sealed.execute(&resolved)?;
    let terminal = report
        .phases
        .iter()
        .all(|run| run.result.outcome().is_completed());

    // The metrics commit, in *both* modes. R2's window is "after the commit,
    // before anything acknowledges", so the commit has to be on the ordinary
    // path too — otherwise the crash-free reference R3 converges against would
    // have no phase row and the convergence claim would be comparing a run
    // that committed metrics with one that never did.
    let mut intent = PhaseMetricIntent::new(mission.clone(), "phase-1", 1, reaped_witness()?);
    intent.status = if terminal {
        orchestrator_app::PhaseStatus::Completed
    } else {
        orchestrator_app::PhaseStatus::Failed
    };
    intent.gate_passed = terminal;
    sealed.record_phase(&intent)?;

    match mode {
        // R1 and R3 — write to terminal and exit cleanly.
        "run" => {
            println!("sidecar terminal={terminal} mission={}", mission.as_str());
            Ok(())
        }
        // R2 — the metrics-commit-before-acknowledgement window: the row above
        // is committed, the marker is published, and the parent kills the whole
        // group before anything acknowledges. `sealed` is a scope-local with a
        // `Drop`, so seal 13 runs only if this process is *not* killed.
        "crash-at-metrics" => {
            let marker = PathBuf::from(std::env::var(SIDECAR_MARKER)?);
            std::fs::write(&marker, b"parked\n")?;
            sleep_until_killed()
        }
        other => Err(format!("unknown sidecar mode {other}").into()),
    }
}

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

fn mission_source() -> String {
    format!("PHASE: build | OBJECTIVE: run the attested helper | RUNTIME: {FIXTURE_RUNTIME}\n")
}

fn sleep_until_killed() -> TestResult {
    let deadline = Instant::now() + PARK_LIMIT;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("the parked sidecar was never killed".into())
}

/// The attested helper every enrolled phase runs. Returns immediately and
/// writes nothing.
#[test]
fn helper_quick_entrypoint() {}

// ---------------------------------------------------------------------------
// The reaping witness `PhaseMetricIntent::new` requires
// ---------------------------------------------------------------------------

fn reaped_witness() -> TestResult<orchestrator_app::ExactProcessGroupAbsence> {
    use orchestrator_app::{
        KernelProcessIdentity, RecordedProcessIdentityStatus, inspect_recorded_process_identity,
    };
    use std::io::BufRead;

    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "reaping_witness_entrypoint", "--nocapture"])
        .env("NANIKA_B5_ROLLBACK_WITNESS", "1")
        .env_remove(SIDECAR_MODE)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.process_group(0);
    let mut child = command.spawn()?;
    let pid = child.id();
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

/// Inert unless the witness variable is set.
#[test]
fn reaping_witness_entrypoint() {
    if std::env::var("NANIKA_B5_ROLLBACK_WITNESS").is_err() {
        return;
    }
    println!("ready");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    std::process::exit(0);
}

// ---------------------------------------------------------------------------
// The crash mechanism (R2)
// ---------------------------------------------------------------------------

/// Spawns the sidecar in its own process group, waits for its durable marker,
/// then `SIGKILL`s the whole group.
///
/// The group rather than the child: a sidecar that is mid-phase may have a live
/// attested helper, and killing only the child would orphan it.
fn crash_the_sidecar(ladder: &mut Ladder, marker: &Path) -> TestResult<Observation> {
    let root = ladder.root.to_string_lossy().into_owned();
    let parent = ladder.parent.to_string_lossy().into_owned();
    let marker_text = marker.to_string_lossy().into_owned();
    let mut command = Command::new(ladder.prefix.selector());
    command
        .args(HELPER_QUICK_SIDECAR)
        .current_dir(&ladder.parent)
        .env_clear()
        .env("HOME", ladder.parent.join("go-user"))
        .env("ORCHESTRATOR_CONFIG_DIR", &ladder.root)
        .env("ORCHESTRATOR_PERSONAS_DIR", ladder.personas())
        .env("PATH", ladder.parent.join("empty-path"))
        .env("TMPDIR", &ladder.parent)
        .env(SIDECAR_MODE, "crash-at-metrics")
        .env(SIDECAR_ROOT, &root)
        .env(SIDECAR_PARENT, &parent)
        .env(SIDECAR_MISSION, RUST_MISSION)
        .env(SIDECAR_MARKER, &marker_text)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command.process_group(0);
    let mut child = command.spawn()?;

    if !await_marker(marker, &mut child)? {
        let _ = child.kill();
        let _ = child.wait();
        return Err("the sidecar never reached its durable metrics barrier".into());
    }
    kill_group(child.id());
    let output = child.wait_with_output()?;
    ladder.reaped.push(format!(
        "r2-crash: exit={:?} signal={:?}",
        output.status.code(),
        output.status.signal()
    ));
    Ok(Observation {
        exit_code: output.status.code(),
        signal: output.status.signal(),
        stdout: Vec::new(),
        stderr: output.stderr,
    })
}

fn await_marker(marker: &Path, child: &mut Child) -> TestResult<bool> {
    let deadline = Instant::now() + BARRIER_DEADLINE;
    while Instant::now() < deadline {
        if marker.exists() {
            return Ok(true);
        }
        if let Some(status) = child.try_wait()? {
            let mut reason = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                use std::io::Read;
                let _ = stderr.read_to_string(&mut reason);
            }
            return Err(format!(
                "the sidecar exited early with {status:?}: {}",
                reason.trim()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(false)
}

fn kill_group(pid: u32) {
    let raw = i32::try_from(pid).unwrap_or_default();
    if let Some(pid) = rustix::process::Pid::from_raw(raw) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
}

// ===========================================================================
// R0 — Go reads the seeded home, through the selector
// ===========================================================================

#[test]
fn r0_the_oracle_reads_the_seeded_home_through_the_selector() -> TestResult {
    let mut ladder = Ladder::new("r0")?;
    ladder.assert_selected(&ladder.go.clone(), "packaged Go")?;

    let status = ladder.leg("r0-status", &["status"], &[])?;
    assert_eq!(
        status.exit_code,
        Some(0),
        "status: {}{}",
        status.stdout_text(),
        status.stderr_text()
    );
    let (stdout, _) = ladder.masked(&status);
    assert!(
        stdout.contains(SEED_MISSION),
        "the oracle did not read the seeded workspace: {stdout}"
    );

    let listed = ladder.leg("r0-events-list", &["events", "list"], &[])?;
    assert_eq!(listed.exit_code, Some(0), "{}", listed.stderr_text());
    assert!(
        listed.stdout_text().contains(SEED_MISSION),
        "events list omitted the seeded log: {}",
        listed.stdout_text()
    );

    let replayed = ladder.leg("r0-events-replay", &["events", "replay", SEED_MISSION], &[])?;
    assert_eq!(replayed.exit_code, Some(0), "{}", replayed.stderr_text());
    let replay_text = replayed.stdout_text();
    for expected in ["mission.started", "mission.completed"] {
        assert!(
            replay_text.contains(expected),
            "events replay omitted {expected}: {replay_text}"
        );
    }

    // The seeded pre-state is itself a comparable artifact: the event facts
    // are exactly the two the manifest seeded, in order.
    assert_eq!(
        ladder.events(SEED_MISSION),
        vec![
            ("mission.started".to_owned(), String::new()),
            ("mission.completed".to_owned(), String::new()),
        ],
    );
    Ok(())
}

// ===========================================================================
// R1 — Rust writes one mission to terminal
// ===========================================================================

#[test]
fn r1_the_rust_sidecar_writes_a_mission_to_terminal() -> TestResult {
    let mut ladder = Ladder::new("r1")?;
    ladder.upgrade_to_rust()?;
    ladder.assert_selected(&ladder.rust.clone(), "Rust sidecar")?;

    let run = ladder.sidecar("r1-run", "run", RUST_MISSION)?;
    assert_eq!(
        run.exit_code,
        Some(0),
        "the sidecar did not complete: {}{}",
        run.stdout_text(),
        run.stderr_text()
    );
    assert!(
        run.stdout_text().contains("sidecar terminal=true"),
        "the sidecar did not reach a terminal outcome: {}",
        run.stdout_text()
    );

    // Post-state, in all three durable classes.
    let events = ladder.events(RUST_MISSION);
    assert!(
        !events.is_empty(),
        "the sidecar wrote no canonical events for {RUST_MISSION}"
    );
    let metrics = ladder.metrics_rows()?;
    assert!(
        metrics.iter().any(|row| row.contains(RUST_MISSION)),
        "the sidecar wrote no metrics row for {RUST_MISSION}: {metrics:#?}"
    );
    let runtime = ladder.runtime_rows()?;
    assert!(
        !runtime.is_empty(),
        "the sidecar left no runtime store rows"
    );

    // The seeded pre-state is untouched by the Rust write.
    assert_eq!(
        ladder.events(SEED_MISSION),
        vec![
            ("mission.started".to_owned(), String::new()),
            ("mission.completed".to_owned(), String::new()),
        ],
        "the Rust write disturbed the seeded event log",
    );
    Ok(())
}

// ===========================================================================
// R2 — Rust crashes at the metrics-commit-before-acknowledgement window
// ===========================================================================

#[test]
fn r2_the_rust_sidecar_dies_under_sigkill_at_the_metrics_window() -> TestResult {
    let mut ladder = Ladder::new("r2")?;
    ladder.upgrade_to_rust()?;
    let marker = ladder.parent.join("parked-metrics");

    let crashed = crash_the_sidecar(&mut ladder, &marker)?;
    assert_eq!(
        crashed.signal,
        Some(9),
        "the sidecar exited with code {:?} instead of dying under SIGKILL, so nothing here \
         witnesses a crash: {}",
        crashed.exit_code,
        crashed.stderr_text()
    );
    assert_eq!(
        crashed.exit_code, None,
        "a signalled death has no exit code",
    );
    assert!(
        marker.is_file(),
        "the crash landed before the metrics commit, so the window under test was never reached"
    );

    // The committed row survived the death: this is what R3 restarts from.
    let metrics = ladder.metrics_rows()?;
    assert!(
        metrics.iter().any(|row| row.contains(RUST_MISSION)),
        "the metrics commit did not survive the SIGKILL: {metrics:#?}"
    );
    Ok(())
}

// ===========================================================================
// R3 — Rust restarts, recovers and completes
// ===========================================================================

#[test]
fn r3_the_rust_sidecar_restarts_recovers_and_completes() -> TestResult {
    // A crash-free reference over its own home, so "converged" is measured
    // rather than asserted from the shape of the code.
    let reference = {
        let mut clean = Ladder::new("r3-reference")?;
        clean.upgrade_to_rust()?;
        let run = clean.sidecar("reference-run", "run", RUST_MISSION)?;
        assert_eq!(run.exit_code, Some(0), "{}", run.stderr_text());
        (clean.events(RUST_MISSION), clean.metrics_rows()?)
    };

    let mut ladder = Ladder::new("r3")?;
    ladder.upgrade_to_rust()?;
    let marker = ladder.parent.join("parked-metrics");
    let crashed = crash_the_sidecar(&mut ladder, &marker)?;
    assert_eq!(crashed.signal, Some(9), "{}", crashed.stderr_text());

    // No orphan lease: the restart takes the same authority in a new process.
    let restart = ladder.sidecar("r3-restart", "run", RUST_MISSION)?;
    assert_eq!(
        restart.exit_code,
        Some(0),
        "the restart did not complete: {}{}",
        restart.stdout_text(),
        restart.stderr_text()
    );
    assert!(restart.stdout_text().contains("sidecar terminal=true"));

    // The restart *re-executes* the phase rather than resuming past it: the
    // composition root's `execute` runs the resolved plan and there is no
    // resume-from-checkpoint on this path, so the canonical log carries a
    // second attempt. Pinned as the exact doubled sequence rather than
    // smoothed over — a change in either direction fails. What converges is
    // the metrics row set below, because the phase row is keyed on
    // mission+phase and upserted (`metrics_owner.rs`'s
    // `ON CONFLICT(id) DO UPDATE`), which is the same property Gate 1b's C4
    // asserts on the Rust-only side of this window.
    let mut doubled = reference.0.clone();
    doubled.extend(reference.0.iter().cloned());
    assert_eq!(
        ladder.events(RUST_MISSION),
        doubled,
        "the restart after the metrics-window crash must append exactly one more attempt",
    );
    // The phase row is the idempotence claim §4 names: "a resume must not
    // create a duplicate phase row". It is keyed on mission+phase and upserted
    // (`metrics_owner.rs`'s `row_key` deliberately excludes the attempt), so a
    // restart must leave it cell for cell identical to the crash-free run's.
    let recovered = ladder.metrics_rows()?;
    assert_eq!(
        rows_named("phases", &recovered),
        rows_named("phases", &reference.1),
        "the restart duplicated or altered the phase row",
    );
    assert_eq!(
        rows_named("phases", &recovered).len(),
        1,
        "the restart left more than one phase row: {recovered:#?}",
    );
    assert_eq!(
        rows_named("missions", &recovered).len(),
        1,
        "the restart left more than one mission row: {recovered:#?}",
    );

    // The mission row's aggregate counters legitimately differ, and the reason
    // is recorded rather than masked away: `record_terminal` computes them as
    // `SELECT count(*) … FROM phases WHERE mission_id = ?`, and the enrolled
    // `execute` writes the terminal row *before* this gate's phase row is
    // committed. So the crash-free run's mission row counts zero phases and
    // the restarted run's counts the attempt the crash already committed. The
    // differing fields are pinned exactly: any other field moving, or these two
    // agreeing, fails and must be re-frozen.
    let differing = differing_fields(
        rows_named("missions", &reference.1)
            .first()
            .ok_or("the reference has no mission row")?,
        rows_named("missions", &recovered)
            .first()
            .ok_or("the recovered home has no mission row")?,
    );
    assert_eq!(
        differing,
        vec!["phases_completed".to_owned(), "phases_total".to_owned()],
        "the restart moved mission-row fields other than the two `record_terminal` recomputes",
    );

    // §3.4's R3 also captures the publication counts the store carries.
    let root = IsolatedFixtureRoot::identify(&ladder.root)?;
    let store = orchestrator_app::open_fixture_runtime_store(&root)?;
    let counts = store.publication_counts()?;
    println!(
        "r3 publication counts: pending={} delivered={} dead_letter={}",
        counts.pending, counts.delivered, counts.dead_letter
    );
    assert_eq!(
        counts.dead_letter, 0,
        "a recovered run must leave no dead-lettered publication",
    );
    Ok(())
}

// ===========================================================================
// R4 — rollback: the packaged Go binary reads and continues the Rust home
// ===========================================================================

/// Builds a home that Rust has written and recovered from a crash, and rolls
/// the selector back to the packaged Go binary.
fn rust_written_then_rolled_back(label: &str) -> TestResult<Ladder> {
    let mut ladder = Ladder::new(label)?;
    ladder.upgrade_to_rust()?;
    let marker = ladder.parent.join("parked-metrics");
    let crashed = crash_the_sidecar(&mut ladder, &marker)?;
    assert_eq!(crashed.signal, Some(9), "{}", crashed.stderr_text());
    let restart = ladder.sidecar("restart", "run", RUST_MISSION)?;
    assert_eq!(restart.exit_code, Some(0), "{}", restart.stderr_text());

    ladder.roll_back_to_go()?;
    Ok(ladder)
}

#[test]
fn r4_rollback_selects_the_packaged_go_binary_and_it_reads_the_rust_home() -> TestResult {
    let mut ladder = rust_written_then_rolled_back("r4")?;
    ladder.assert_selected(&ladder.go.clone(), "packaged Go")?;

    // `status` is pinned on its own by
    // [`a_rust_authored_checkpoint_carries_a_plan_the_rolled_back_go_binary_reads`],
    // which is where the plan-in-checkpoint claim and Go's own nil-check
    // defect (TRK-1291) are both asserted. Repeating it here would duplicate
    // that row without adding a rollback fact.
    let listed = ladder.leg("r4-events-list", &["events", "list"], &[])?;
    assert_eq!(listed.exit_code, Some(0), "{}", listed.stderr_text());
    let listed_text = listed.stdout_text();
    for mission in [SEED_MISSION, RUST_MISSION] {
        assert!(
            listed_text.contains(mission),
            "go's events list omitted {mission} after rollback: {listed_text}"
        );
    }

    let replayed = ladder.leg("r4-events-replay", &["events", "replay", RUST_MISSION], &[])?;
    assert_eq!(
        replayed.exit_code,
        Some(0),
        "go could not replay the rust-written log: {}",
        replayed.stderr_text()
    );
    let rust_facts = ladder.events(RUST_MISSION);
    let replay_text = replayed.stdout_text();
    for (event_type, _) in &rust_facts {
        assert!(
            replay_text.contains(event_type),
            "go's replay omitted the rust-written {event_type}: {replay_text}"
        );
    }

    // The Rust-written metrics rows are readable by the rolled-back Go binary,
    // cell for cell rather than by count.
    let before = ladder.metrics_rows()?;
    let metrics = ladder.leg("r4-metrics", &["metrics"], &[])?;
    assert_eq!(metrics.exit_code, Some(0), "{}", metrics.stderr_text());
    assert!(
        metrics.stdout_text().contains(RUST_MISSION),
        "go's metrics did not list the rust-written mission: {}",
        metrics.stdout_text()
    );
    assert_eq!(
        ladder.metrics_rows()?,
        before,
        "go's read after rollback changed the rust-written rows",
    );
    Ok(())
}

#[test]
fn r4_the_rolled_back_go_binary_continues_the_rust_home_to_a_second_terminal() -> TestResult {
    let mut ladder = rust_written_then_rolled_back("r4-continue")?;
    let workspace = ladder
        .root
        .join("workspaces")
        .join(SEED_MISSION)
        .to_string_lossy()
        .into_owned();

    let resumed = ladder.leg("r4-resume", &["run", "--resume", &workspace], &[])?;
    assert_eq!(
        resumed.exit_code,
        Some(0),
        "the rolled-back Go binary could not continue the home: {}{}",
        resumed.stdout_text(),
        resumed.stderr_text()
    );
    let (stdout, _) = ladder.masked(&resumed);
    for expected in ["resuming mission from", "mission completed"] {
        assert!(
            stdout.contains(expected),
            "the continuation did not take the resume path: {stdout}"
        );
    }
    assert!(
        stdout.contains("2 completed, 0 failed"),
        "the continuation changed the phase outcomes: {stdout}"
    );

    // Still Go, still through the link: the gate never leaves Rust selected.
    ladder.assert_selected(&ladder.go.clone(), "packaged Go")?;
    Ok(())
}

// ===========================================================================
// R5 — a second Go read converges with R4
// ===========================================================================

#[test]
fn r5_a_second_go_read_after_the_continuation_converges_with_r4() -> TestResult {
    let mut ladder = rust_written_then_rolled_back("r5")?;
    let workspace = ladder
        .root
        .join("workspaces")
        .join(SEED_MISSION)
        .to_string_lossy()
        .into_owned();
    let resumed = ladder.leg("r5-resume", &["run", "--resume", &workspace], &[])?;
    assert_eq!(resumed.exit_code, Some(0), "{}", resumed.stderr_text());

    let first = ladder.leg("r5-read-1", &["events", "list"], &[])?;
    let first_rows = ladder.metrics_rows()?;
    let first_events = ladder.events(SEED_MISSION);
    let second = ladder.leg("r5-read-2", &["events", "list"], &[])?;
    let second_rows = ladder.metrics_rows()?;
    let second_events = ladder.events(SEED_MISSION);

    assert_eq!(first.exit_code, second.exit_code);
    assert_eq!(
        ladder.masked(&first),
        ladder.masked(&second),
        "two reads of the same home after the continuation must agree byte for byte once the \
         declared volatile fields are masked",
    );
    assert_eq!(
        first_rows, second_rows,
        "a read changed a row: the reads are not converged",
    );
    assert_eq!(first_events, second_events);
    Ok(())
}

// ===========================================================================
// Negative assertions (§4, Gate 2)
// ===========================================================================

#[test]
fn a_missing_oracle_is_a_hard_failure_naming_the_manifest_and_the_candidate() -> TestResult {
    let manifest = support::frozen_tree_manifest();
    let built = workspace_root().join("target/go-oracle/orchestrator");
    let absent = Path::new("/nonexistent/orchestrator-go-oracle");

    let Err(message) = accept_cached_oracle(absent, &built, "unused", &manifest) else {
        return Err("an absent oracle was accepted".into());
    };
    for expected in [
        "does not name a file".to_owned(),
        absent.display().to_string(),
        built.display().to_string(),
        manifest.display().to_string(),
    ] {
        assert!(
            message.contains(&expected),
            "the refusal did not name {expected:?}: {message}"
        );
    }
    // And the resolver itself never yields a skip: it is a `Result`, and every
    // rung propagates it with `?`.
    assert!(
        !message.contains("skip"),
        "the refusal reads like a skip: {message}"
    );
    Ok(())
}

#[test]
fn a_mismatched_oracle_digest_is_refused_naming_both_digests() -> TestResult {
    let manifest = support::frozen_tree_manifest();
    let built = workspace_root().join("target/go-oracle/orchestrator");
    // A real, readable file that is certainly not the in-tree Go build.
    let decoy = std::env::current_exe()?;
    let reference = "0".repeat(64);

    let Err(message) = accept_cached_oracle(&decoy, &built, &reference, &manifest) else {
        return Err("a decoy binary was accepted as the frozen Go oracle".into());
    };
    let observed = sha256_file(&decoy)?;
    for expected in [
        reference,
        observed,
        decoy.display().to_string(),
        built.display().to_string(),
    ] {
        assert!(
            message.contains(&expected),
            "the refusal did not name {expected:?}: {message}"
        );
    }
    Ok(())
}

#[test]
fn the_leg_allowlist_never_carries_live_home_enrollment() -> TestResult {
    assert!(
        !LEG_ENVIRONMENT.contains(&"NANIKA_LIVE_HOME_ENROLL"),
        "the leg allowlist carries live-home enrollment: {LEG_ENVIRONMENT:?}",
    );
    // And the process running this gate must not be carrying it either, since
    // `env_clear` plus this allowlist is what every leg is given.
    assert!(
        std::env::var_os("NANIKA_LIVE_HOME_ENROLL").is_none()
            || LEG_ENVIRONMENT.contains(&"NANIKA_LIVE_HOME_ENROLL"),
        "NANIKA_LIVE_HOME_ENROLL is set in this process and would need an explicit rule",
    );
    Ok(())
}

#[test]
fn the_live_home_is_never_touched() -> TestResult {
    // The live home's identity before and after a full ladder. `HOME` for
    // every leg points inside the fixture parent, so the real one must not
    // even be stat-able differently afterwards.
    let live = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".alluka"));
    let before = live.as_ref().and_then(|path| std::fs::metadata(path).ok());
    let before = before.map(|meta| (meta.len(), meta.modified().ok()));

    let mut ladder = rust_written_then_rolled_back("live-home")?;
    let listed = ladder.leg("live-home-events", &["events", "list"], &[])?;
    assert_eq!(listed.exit_code, Some(0), "{}", listed.stderr_text());

    let after = live.as_ref().and_then(|path| std::fs::metadata(path).ok());
    let after = after.map(|meta| (meta.len(), meta.modified().ok()));
    assert_eq!(
        before, after,
        "the ladder changed the live home's identity; every leg must stay inside its fixture",
    );

    // Nothing the ladder created lives outside its own parent.
    assert!(
        ladder.root.starts_with(&ladder.parent),
        "the fixture root escaped its parent",
    );
    Ok(())
}

#[test]
fn every_leg_is_reaped_before_the_next_spawns() -> TestResult {
    let mut ladder = Ladder::new("reaped")?;
    ladder.upgrade_to_rust()?;
    let run = ladder.sidecar("reaped-run", "run", RUST_MISSION)?;
    assert_eq!(run.exit_code, Some(0), "{}", run.stderr_text());
    ladder.roll_back_to_go()?;
    let listed = ladder.leg("reaped-events", &["events", "list"], &[])?;
    assert_eq!(listed.exit_code, Some(0), "{}", listed.stderr_text());

    // Every leg recorded a `wait()` result, which only happens after the child
    // has been reaped; the count equals the number of legs spawned, so no leg
    // was ever left running while the next started.
    assert_eq!(
        ladder.reaped.len(),
        2,
        "the recorded reaping log does not cover every leg: {:?}",
        ladder.reaped
    );
    for entry in &ladder.reaped {
        assert!(
            entry.contains("exit=") && entry.contains("signal="),
            "a leg was recorded without a wait() result: {entry}"
        );
    }
    println!("reaped in order: {:?}", ladder.reaped);
    Ok(())
}

#[test]
fn a_single_cell_mutation_fails_the_row_comparison() -> TestResult {
    let mut ladder = Ladder::new("mutation")?;
    ladder.upgrade_to_rust()?;
    let run = ladder.sidecar("mutation-run", "run", RUST_MISSION)?;
    assert_eq!(run.exit_code, Some(0), "{}", run.stderr_text());

    let before = ladder.metrics_rows()?;
    assert!(!before.is_empty(), "there is no row to mutate");

    // Change exactly one cell, leaving the row count identical.
    {
        let connection = rusqlite::Connection::open(ladder.root.join("metrics.db"))?;
        let changed = connection.execute(
            "UPDATE missions SET domain = 'mutated' WHERE id = ?1",
            [RUST_MISSION],
        )?;
        assert_eq!(changed, 1, "the mutation did not land on exactly one row");
    }

    let after = ladder.metrics_rows()?;
    assert_eq!(
        before.len(),
        after.len(),
        "the mutation changed the row count, so this would not prove counts are insufficient",
    );
    assert_ne!(
        before, after,
        "a single-cell mutation left the ordered row set unchanged; the comparison is only \
         counting rows",
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The one divergence rollback exposes, pinned in three halves
// ---------------------------------------------------------------------------

/// `internal/cmd/status.go` dereferences `cp.Plan` without a nil check, so any
/// checkpoint whose `plan` key is absent crashes `orchestrator status` with a
/// nil-pointer `SIGSEGV` (Go exits 2 after printing the panic).
const GO_STATUS_NIL_PLAN: &str = "internal/cmd/status.go:63";

/// The Go defect rollback exposes, and the Rust repair that keeps rollback off
/// it — asserted in three halves so neither can decay into a test that did not
/// run.
///
/// **Half A — the defect is Go's, and it is still there.** A checkpoint with no
/// `plan`, hand-written by this test with no Rust process involved, crashes the
/// frozen oracle's `status`. The fault is the missing nil check at
/// [`GO_STATUS_NIL_PLAN`], not something the Rust codec emits wrongly, and it
/// is the owner's to fix (TRK-1291) — nothing on the Rust side of this branch
/// edits Go.
///
/// **Half B — Rust no longer leaves that input (CF-M4a-4).** This half used to
/// pin the opposite: `admit_workspace` seeded a plan-less checkpoint, nothing
/// on the enrolled execute path rewrote it, and rollback therefore landed Go on
/// a workspace its own `status` could not survive. The composition root now
/// seeds the empty placeholder plan and `SealedRun::execute` publishes the
/// authored phases into it before the first dispatch, so a completed Rust run
/// leaves a checkpoint whose `plan` carries every released phase — and the
/// rolled-back Go binary's `status` exits 0 over it and counts those phases.
///
/// **Half C — the blast radius is still measured.** `events list`, `events
/// replay` and `metrics` read the same home and exit 0, which is what keeps
/// R4's rollback claim a claim about the boundary rather than about one
/// command.
///
/// A change on either side must update this row rather than silently turn a
/// crash into a pass, or a pass back into a crash.
#[test]
fn a_rust_authored_checkpoint_carries_a_plan_the_rolled_back_go_binary_reads() -> TestResult {
    // --- Half A: Go alone, no Rust process in the case at all.
    let mut go_only = Ladder::new("nil-plan-go")?;
    go_only.assert_selected(&go_only.go.clone(), "packaged Go")?;
    let workspace = go_only.root.join("workspaces").join("no-plan");
    private_tree(&go_only.root, &workspace)?;
    std::fs::write(
        workspace.join("mission.md"),
        b"mission: no-plan
",
    )?;
    std::fs::write(
        workspace.join("checkpoint.json"),
        br#"{"version":1,"payload":{"version":2,"workspace_id":"no-plan","status":"pending"}}"#,
    )?;
    let crashed = go_only.leg("nil-plan-status", &["status"], &[])?;
    assert_eq!(
        crashed.exit_code,
        Some(2),
        "the oracle survived a plan-less checkpoint; GO_STATUS_NIL_PLAN is stale and this row          must be re-frozen: {}{}",
        crashed.stdout_text(),
        crashed.stderr_text()
    );
    let panic_text = crashed.stderr_text();
    for expected in [
        "nil pointer dereference",
        GO_STATUS_NIL_PLAN,
        "cmd.showStatus",
    ] {
        assert!(
            panic_text.contains(expected),
            "the oracle's crash did not name {expected:?}: {panic_text}"
        );
    }

    // --- Half B: what a completed Rust run actually leaves behind.
    let mut rust = Ladder::new("nil-plan-rust")?;
    rust.upgrade_to_rust()?;
    let run = rust.sidecar("nil-plan-run", "run", RUST_MISSION)?;
    assert_eq!(run.exit_code, Some(0), "{}", run.stderr_text());
    assert!(run.stdout_text().contains("sidecar terminal=true"));

    let written = std::fs::read(
        rust.root
            .join("workspaces")
            .join(RUST_MISSION)
            .join("checkpoint.json"),
    )?;
    let decoded = orchestrator_core::decode_checkpoint(&written)?;
    assert_eq!(decoded.projection.workspace_id, RUST_MISSION);
    let plan = decoded.projection.plan.as_ref().ok_or(
        "the Rust run left a plan-less checkpoint; CF-M4a-4's repair has regressed and rollback \
         lands Go on half A's input again",
    )?;
    assert_eq!(
        plan.id, RUST_MISSION,
        "the published plan is not this mission's",
    );
    assert!(
        !plan.phases.is_empty(),
        "the published plan carries no phases, so Go's `status` would count 0 of them",
    );

    // --- Half C: the same rolled-back binary reads the same home.
    rust.roll_back_to_go()?;
    rust.assert_selected(&rust.go.clone(), "packaged Go")?;
    let status = rust.leg("rust-home-status", &["status"], &[])?;
    assert_eq!(
        status.exit_code,
        Some(0),
        "the rolled-back oracle's status did not survive the Rust-written home: {}{}",
        status.stdout_text(),
        status.stderr_text(),
    );
    assert!(
        !status.stderr_text().contains(GO_STATUS_NIL_PLAN),
        "the oracle still crashed at {GO_STATUS_NIL_PLAN}: {}",
        status.stderr_text(),
    );
    assert!(
        status
            .stdout_text()
            .contains(&format!("/{} phases", plan.phases.len())),
        "the oracle's status did not count the published plan's {} phases: {}",
        plan.phases.len(),
        status.stdout_text(),
    );

    for (name, argv) in [
        ("events-list", vec!["events", "list"]),
        ("events-replay", vec!["events", "replay", RUST_MISSION]),
        ("metrics", vec!["metrics"]),
    ] {
        let observed = rust.leg(name, &argv, &[])?;
        assert_eq!(
            observed.exit_code,
            Some(0),
            "the divergence is wider than `status`: {name} also failed: {}{}",
            observed.stdout_text(),
            observed.stderr_text()
        );
    }
    Ok(())
}

#[test]
fn the_gate_leaves_the_packaged_go_binary_selected() -> TestResult {
    let ladder = rust_written_then_rolled_back("default")?;
    ladder.assert_selected(&ladder.go.clone(), "packaged Go")?;
    // And the Rust sidecar is installed but not selected: the gate exercised
    // the upgrade and then undid it.
    let selected = ladder.prefix.selected()?;
    assert_ne!(
        selected, ladder.rust_target,
        "the gate left the Rust sidecar selected",
    );
    assert_eq!(selected, ladder.go_target);
    Ok(())
}

#[test]
fn the_seed_manifest_is_exactly_the_seed_tree() -> TestResult {
    let root = seed_root();
    let declared: Vec<String> = seed_entries();
    let mut observed = Vec::new();
    walk(&root, &root, &mut observed)?;
    observed.sort();
    let mut sorted = declared.clone();
    sorted.sort();
    assert_eq!(
        sorted, declared,
        "the seed manifest is not sorted, so a diff of it is not stable",
    );
    assert_eq!(
        observed, sorted,
        "the seed manifest and the seed tree disagree; the copied pre-state would not be the \
         artifact the gate claims to seed",
    );
    Ok(())
}

fn walk(root: &Path, directory: &Path, into: &mut Vec<String>) -> TestResult {
    let mut children: Vec<PathBuf> = std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    children.sort();
    for child in children {
        if child.file_name().is_some_and(|name| name == "manifest") {
            continue;
        }
        if child.is_dir() {
            walk(root, &child, into)?;
        } else {
            into.push(child.strip_prefix(root)?.to_string_lossy().into_owned());
        }
    }
    Ok(())
}

#[test]
fn the_masking_rules_are_named_and_only_touch_declared_volatiles() -> TestResult {
    let root = Path::new("/tmp/fixture-root");
    let sample = "seed-mission [completed] 2/2 phases — started 2026-09-03T02:37:51.605672Z \
                  in 25.594667ms as evt_56b0205e8e8071b0 under /tmp/fixture-root/workspaces";
    let masked = mask(sample, &[(root, "<root>")]);
    assert_eq!(
        masked,
        "seed-mission [completed] 2/2 phases — started <timestamp> in <duration> as <event-id> \
         under <root>/workspaces",
    );
    // Nothing that is not a declared volatile is touched: the mission id, the
    // status, and the phase counts survive verbatim.
    for preserved in ["seed-mission", "[completed]", "2/2 phases"] {
        assert!(
            masked.contains(preserved),
            "masking removed {preserved:?}: {masked}"
        );
    }
    Ok(())
}

#[test]
fn load_average_is_recorded_for_this_gate() -> TestResult {
    let output = Command::new("/usr/bin/uptime").output()?;
    let recorded = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    assert!(
        !recorded.is_empty(),
        "the load average must be observable for this gate"
    );
    let path = std::env::temp_dir().join("orchestrator-rs-b5-rollback-load.txt");
    std::fs::write(&path, format!("{recorded}\n"))?;
    println!("gate 2 load average: {recorded}");
    println!("recorded to {}", path.display());
    Ok(())
}
