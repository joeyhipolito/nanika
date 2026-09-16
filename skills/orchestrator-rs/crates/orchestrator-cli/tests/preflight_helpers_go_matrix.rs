//! B5-DESIGN §4, Gate 7 — the preflight section registry, byte-normalized
//! against the frozen in-tree Go oracle.
//!
//! Adapted from `analytics_go_fixture_matrix.rs`: every expectation is a
//! *recorded observation of the Go binary* over a store the harness builds, not
//! a hand-written expected string, and a missing capture is a hard failure with
//! a regeneration hint rather than a skip.
//!
//! It differs from that gate in one respect, and the difference is the point.
//! `analytics` compares Rust to a frozen Go capture because the Rust CLI
//! implements `audit scorecard`. The Rust CLI implements **no** preflight
//! surface — `hooks` resolves to `CliError::UnsupportedCommand`, and no
//! `Section`, `Brief` or `ComposeWithCapacity` equivalent exists anywhere in
//! `crates/` — so there is nothing to compare it to. That absence is recorded
//! here as a **gap row**, not as a skipped test: `the_rust_preflight_surface_is
//! _a_recorded_gap_not_a_skip` pins the exact refusal and simultaneously proves
//! the oracle *does* implement the surface, so the row names which side is
//! missing.
//!
//! What the live half then pins is the Go contract the Rust port must meet:
//! every capture is replayed against the frozen in-tree oracle over a home
//! built exactly as `regenerate.sh` built it, and the two are compared
//! byte-for-byte after two named normalizations —
//!
//! * **FIXTURE-ROOT** — the synthetic home's absolute path becomes `<FIXTURE>`.
//! * **LAST-EVENT** — `last_event: <RFC3339>` becomes `last_event: <MASKED>`,
//!   the mission section's only clock-derived field.
//!
//! and nothing else. A section added, renamed, reordered or re-prioritized, a
//! changed drop order under budget pressure, or a changed JSON block shape all
//! fail. Every volatile input the sections read is pointed at the fixture
//! (`KB_DATA_DIR`, `KB_INDEX_DB`, `KB_VECTORS_FILE`, `NANIKA_JOB_TRACKER_DB`),
//! so the freshness row reads `MISSING` deterministically rather than an age.
//!
//! **The oracle is mandatory.** A missing or unusable Go binary is a hard
//! failure naming exactly what was checked and where — never an `eprintln!` +
//! `Ok(())` green (TRK-1280).

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

mod support;
use support::{frozen_go_oracle, frozen_tree_manifest};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static CASE: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------------------
// The oracle
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Captures
// ---------------------------------------------------------------------------

fn oracle_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/preflight/oracle")
}

/// One recorded, already-normalized Go observation.
struct Capture {
    name: String,
    argv: Vec<String>,
    stdout: String,
    stderr: String,
    exit_code: i32,
}

impl Capture {
    /// Loads one capture.
    ///
    /// A missing member is an `Err`, which fails the calling test — never a
    /// skip. `a_missing_capture_is_a_hard_failure_rather_than_a_skip` checks
    /// that property directly.
    fn load(name: &str) -> TestResult<Self> {
        let directory = oracle_root().join(name);
        let read = |leaf: &str| -> TestResult<String> {
            fs::read_to_string(directory.join(leaf)).map_err(|error| {
                format!(
                    "missing Go oracle capture {}/{leaf}: {error}\n\
                     regenerate with: ORCHESTRATOR_ACCEPTED_GO_BIN=<frozen in-tree go binary> \
                     crates/orchestrator-cli/tests/fixtures/preflight/regenerate.sh",
                    directory.display()
                )
                .into()
            })
        };
        Ok(Self {
            name: name.to_owned(),
            argv: read("argv")?.lines().map(str::to_owned).collect(),
            stdout: read("stdout")?,
            stderr: read("stderr")?,
            exit_code: read("exit")?.trim().parse()?,
        })
    }

    fn all() -> TestResult<Vec<Self>> {
        let root = oracle_root();
        let mut entries = fs::read_dir(&root)
            .map_err(|error| format!("cannot read the capture set {}: {error}", root.display()))?
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        let captures = entries
            .into_iter()
            .filter(|entry| entry.path().is_dir())
            .map(|entry| Self::load(&entry.file_name().to_string_lossy()))
            .collect::<TestResult<Vec<Self>>>()?;
        if captures.is_empty() {
            return Err(format!("the capture set {} is empty", root.display()).into());
        }
        Ok(captures)
    }
}

// ---------------------------------------------------------------------------
// The synthetic home
// ---------------------------------------------------------------------------

/// The home `regenerate.sh` builds, rebuilt here so the replay sees exactly the
/// inputs the capture saw.
struct Fixture {
    parent: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let parent = fs::canonicalize(std::env::temp_dir())?.join(format!(
            "orchestrator-rs-b5-preflight-{}-{number}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir_all(&parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))?;
        }
        let fixture = Self { parent };
        for leaf in ["user", "personas", "empty-path", "kb"] {
            fs::create_dir_all(fixture.parent.join(leaf))?;
        }
        let workspace = fixture.home().join("workspaces/ws-preflight");
        fs::create_dir_all(&workspace)?;
        fs::write(workspace.join("mission.md"), b"a fixture mission\n")?;
        fs::write(
            workspace.join("checkpoint.json"),
            br#"{"version":1,"payload":{"version":2,"workspace_id":"ws-preflight","domain":"dev","plan":{"id":"plan-preflight","task":"fixture task","phases":[{"id":"phase-1","name":"build","status":"completed"},{"id":"phase-2","name":"verify","status":"pending"}]},"status":"in_progress","started_at":"2026-07-13T00:00:00Z"}}"#,
        )?;
        Ok(fixture)
    }

    fn home(&self) -> PathBuf {
        self.parent.join("home")
    }

    /// The `env -i` allowlist, matching `regenerate.sh` variable for variable.
    /// Every clock- or estate-derived input the sections read is aimed inside
    /// the fixture, so `MISSING` is a determinate answer rather than an age.
    fn command(&self, binary: &Path, argv: &[String]) -> Command {
        let mut command = Command::new(binary);
        command
            .args(argv)
            .current_dir(&self.parent)
            .env_clear()
            .env("HOME", self.parent.join("user"))
            .env("ORCHESTRATOR_CONFIG_DIR", self.home())
            .env("ORCHESTRATOR_PERSONAS_DIR", self.parent.join("personas"))
            .env("PATH", self.parent.join("empty-path"))
            .env("TMPDIR", &self.parent)
            .env("KB_DATA_DIR", self.parent.join("kb"))
            .env("KB_INDEX_DB", self.parent.join("kb/index.db"))
            .env(
                "KB_VECTORS_FILE",
                self.parent.join("kb/vault-vectors.kbvec"),
            )
            .env(
                "NANIKA_JOB_TRACKER_DB",
                self.parent.join("kb/job-tracker.db"),
            );
        command
    }

    fn observe(&self, binary: &Path, argv: &[String]) -> io::Result<Observed> {
        let output = self.command(binary, argv).output()?;
        Ok(Observed {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: self.normalize(&String::from_utf8_lossy(&output.stdout)),
            stderr: self.normalize(&String::from_utf8_lossy(&output.stderr)),
        })
    }

    /// The two named normalization rules, and only those.
    ///
    /// B5-DESIGN §3.4's rule 1: "no normalization beyond the documented
    /// volatile fields, each of which is masked by an explicit named rule, not
    /// a regex sweep."
    fn normalize(&self, text: &str) -> String {
        // FIXTURE-ROOT.
        let text = text.replace(&self.parent.to_string_lossy().into_owned(), "<FIXTURE>");
        // LAST-EVENT: the mission section renders one clock-derived field.
        let mut normalized = String::with_capacity(text.len());
        let mut rest = text.as_str();
        const MARKER: &str = "last_event: ";
        while let Some(index) = rest.find(MARKER) {
            normalized.push_str(&rest[..index + MARKER.len()]);
            normalized.push_str("<MASKED>");
            rest = &rest[index + MARKER.len()..];
            let end = rest
                .find(|character: char| {
                    !(character.is_ascii_digit() || "TZ:.+-".contains(character))
                })
                .unwrap_or(rest.len());
            rest = &rest[end..];
        }
        normalized.push_str(rest);
        normalized
    }
}

struct Observed {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn rust_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_orchestrator"))
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
fn every_frozen_capture_is_reproduced_byte_for_byte_after_normalization() -> TestResult {
    let go_binary = frozen_go_oracle()?;
    let captures = Capture::all()?;
    for expected in [
        "default",
        "json",
        "sections-mission-kb",
        "sections-scheduler",
        "budget-truncated",
        "budget-single-section",
        "help",
    ] {
        assert!(
            captures.iter().any(|capture| capture.name == expected),
            "the {expected} capture disappeared from the oracle"
        );
    }
    for capture in &captures {
        let fixture = Fixture::new(&capture.name)?;
        let observed = fixture.observe(&go_binary, &capture.argv)?;
        assert_eq!(
            observed.exit_code, capture.exit_code,
            "{} exit status",
            capture.name
        );
        assert_eq!(
            observed.stdout, capture.stdout,
            "{} stdout diverged from the frozen Go capture",
            capture.name
        );
        assert_eq!(
            observed.stderr, capture.stderr,
            "{} stderr diverged from the frozen Go capture",
            capture.name
        );
    }
    Ok(())
}

#[test]
fn the_registry_renders_every_section_in_priority_order() -> TestResult {
    // The `--format json` capture is the registry itself: `BuildBrief` sets
    // each block's `name` from `Section.Name()` and `List()` sorts ascending by
    // `Priority()` with registration order breaking ties, so the block sequence
    // *is* the priority order. Pinning it here means a section added, removed,
    // renamed or re-prioritized fails this gate by name rather than by a byte
    // diff nobody can read.
    let json = Capture::load("json")?;
    let value: serde_json::Value = serde_json::from_str(&json.stdout)?;
    let names: Vec<&str> = value
        .get("blocks")
        .and_then(serde_json::Value::as_array)
        .ok_or("the json capture has no blocks array")?
        .iter()
        .filter_map(|block| block.get("name").and_then(serde_json::Value::as_str))
        .collect();
    assert_eq!(
        names,
        [
            "mission",
            "scheduler",
            "nen",
            "slack_health",
            "tracker",
            "obsidian",
            "kb",
            "learnings",
        ],
        "the preflight section registry changed shape or order"
    );

    // And the text render carries the same sections, in the same order, for
    // every block whose body is non-empty — `RenderMarkdown` skips empty ones.
    let default = Capture::load("default")?;
    let mut previous = 0_usize;
    for block in value
        .get("blocks")
        .and_then(serde_json::Value::as_array)
        .ok_or("blocks")?
    {
        let body = block
            .get("body")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if body.trim().is_empty() {
            continue;
        }
        let title = block
            .get("title")
            .and_then(serde_json::Value::as_str)
            .ok_or("block title")?;
        let heading = format!("### {title}");
        let index = default
            .stdout
            .find(&heading)
            .ok_or_else(|| format!("the text render omits the non-empty section {title:?}"))?;
        assert!(
            index >= previous,
            "the text render puts {title:?} out of registry order"
        );
        previous = index;
    }
    Ok(())
}

#[test]
fn the_budget_drop_order_is_lowest_priority_first() -> TestResult {
    // B5-DESIGN §4 Gate 7's negative assertion: "the priority drop order under
    // budget pressure is identical". `ComposeWithCapacity` drops from the end
    // of the priority-ordered block list, so the dropped names must be a
    // suffix of the registry order, reported in that order.
    let truncated = Capture::load("budget-truncated")?;
    let dropped = truncated
        .stderr
        .lines()
        .find_map(|line| line.strip_prefix("preflight: dropped sections to fit capacity: "))
        .ok_or("the truncation capture reports no dropped sections")?
        .split(", ")
        .map(str::trim)
        .collect::<Vec<&str>>();
    assert_eq!(
        dropped,
        ["learnings", "kb"],
        "the budget drop order changed"
    );

    let registry: Vec<String> =
        serde_json::from_str::<serde_json::Value>(&Capture::load("json")?.stdout)?
            .get("blocks")
            .and_then(serde_json::Value::as_array)
            .ok_or("blocks")?
            .iter()
            .filter_map(|block| {
                block
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
    let suffix: Vec<&str> = registry
        .iter()
        .rev()
        .take(dropped.len())
        .map(String::as_str)
        .collect();
    assert_eq!(
        dropped, suffix,
        "the dropped sections are not the lowest-priority suffix of the registry"
    );

    // Under a budget too small for even the highest-priority section, every
    // other section is dropped and the survivor's body is truncated — never
    // an empty brief.
    let single = Capture::load("budget-single-section")?;
    assert!(
        single.stdout.contains("## Operational Pre-flight"),
        "the single-section budget produced no brief at all"
    );
    assert!(
        single.stdout.len() < truncated.stdout.len() + 1,
        "the tighter budget did not shrink the brief"
    );
    Ok(())
}

#[test]
fn suppression_produces_zero_bytes_in_the_oracle() -> TestResult {
    // The other Gate 7 negative assertion. It is asserted live rather than
    // captured because it needs one extra environment variable, and a
    // zero-byte capture file is indistinguishable from a capture that failed
    // to record.
    let go_binary = frozen_go_oracle()?;
    let fixture = Fixture::new("no-inject")?;
    let argv = vec!["hooks".to_owned(), "preflight".to_owned()];
    let output = fixture
        .command(&go_binary, &argv)
        .env("NANIKA_NO_INJECT", "1")
        .output()?;
    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stdout.is_empty(),
        "NANIKA_NO_INJECT=1 still produced {} bytes",
        output.stdout.len()
    );
    // Anti-vacuity: the same fixture without the variable does produce a brief,
    // so the emptiness above is suppression rather than an empty registry.
    let unsuppressed = fixture.observe(&go_binary, &argv)?;
    assert!(
        unsuppressed.stdout.contains("## Operational Pre-flight"),
        "the fixture produces no brief at all, so suppression proves nothing"
    );
    Ok(())
}

#[test]
fn json_round_trips_and_agrees_with_the_text_render() -> TestResult {
    let json = Capture::load("json")?;
    let value: serde_json::Value = serde_json::from_str(&json.stdout)?;
    let reencoded = serde_json::to_string(&value)?;
    let reparsed: serde_json::Value = serde_json::from_str(&reencoded)?;
    assert_eq!(reparsed, value, "the JSON brief does not round-trip");

    let default = Capture::load("default")?;
    for block in value
        .get("blocks")
        .and_then(serde_json::Value::as_array)
        .ok_or("blocks")?
    {
        let body = block
            .get("body")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if body.trim().is_empty() {
            continue;
        }
        for line in body.lines().filter(|line| !line.trim().is_empty()) {
            assert!(
                default.stdout.contains(line),
                "the JSON body carries a line the text render does not: {line:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn the_rust_preflight_surface_is_a_recorded_gap_not_a_skip() -> TestResult {
    // The row this gate exists to record. `hooks` is in `is_go_command`, so the
    // Rust CLI refuses it; no `Section`/`Brief`/`ComposeWithCapacity`
    // equivalent exists in `crates/`. Both halves are asserted, so the row
    // cannot decay into "the test did not run": the oracle must implement the
    // surface, and the Rust side must refuse it in the exact shape pinned here.
    let go_binary = frozen_go_oracle()?;
    let fixture = Fixture::new("gap")?;
    for argv in [
        vec!["hooks".to_owned(), "preflight".to_owned()],
        vec![
            "hooks".to_owned(),
            "preflight".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
        ],
        // `--help` rather than a bare `inject-context`: with an empty
        // learnings database the oracle emits zero bytes, which cannot witness
        // a *Rust* gap.
        vec![
            "hooks".to_owned(),
            "inject-context".to_owned(),
            "--help".to_owned(),
        ],
        vec!["hooks".to_owned(), "--help".to_owned()],
    ] {
        let go = fixture.observe(&go_binary, &argv)?;
        assert_eq!(
            go.exit_code, 0,
            "{argv:?}: the oracle refused its own surface"
        );
        assert!(
            !go.stdout.is_empty(),
            "{argv:?}: the oracle produced nothing, so this is not a Rust gap"
        );

        let rust = fixture.observe(&rust_binary(), &argv)?;
        assert_eq!(
            rust.exit_code, 1,
            "{argv:?}: the refusal's exit status moved"
        );
        assert!(
            rust.stdout.is_empty(),
            "{argv:?}: a refusing command wrote to stdout"
        );
        assert_eq!(
            rust.stderr.trim(),
            "unsupported architecture-foundation command",
            "{argv:?}: the refusal's wording moved"
        );
    }
    Ok(())
}

#[test]
fn a_missing_capture_is_a_hard_failure_rather_than_a_skip() {
    assert!(
        Capture::load("no-such-case").is_err(),
        "a missing capture did not fail the gate"
    );
    assert!(
        Capture::load("default").is_ok(),
        "the loader rejects a capture that exists"
    );
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

#[test]
fn the_captures_still_carry_the_strings_gate_7_names() -> TestResult {
    // Guards the byte comparison against a capture set that quietly loses the
    // wording B5-DESIGN §4 Gate 7 depends on: byte equality against a drifted
    // capture would still pass.
    assert!(
        Capture::load("default")?
            .stdout
            .starts_with("## Operational Pre-flight"),
        "the default capture lost the brief's main heading"
    );
    assert!(
        Capture::load("help")?
            .stdout
            .contains("Set NANIKA_NO_INJECT=1 to suppress output entirely."),
        "the help capture lost the suppression contract"
    );
    assert!(
        Capture::load("help")?.stdout.contains("(default 6144)"),
        "the help capture lost the documented 6 KB default budget"
    );
    assert!(
        Capture::load("default")?
            .stdout
            .contains("freshness: mirror MISSING"),
        "the fixture stopped aiming the kb freshness probe inside itself"
    );
    Ok(())
}
