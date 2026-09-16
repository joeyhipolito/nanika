//! B5-DESIGN §4, Gate 3 — every no-effect CLI surface, censused against the
//! frozen in-tree Go oracle.
//!
//! The shape is `learning_hooks_dry_run_matrix.rs`'s (argv comes out of a
//! capture directory, so a case added to the corpus is covered without editing
//! this file) crossed with `go_cli_differential.rs`'s (an [`Observation`] of
//! stdout bytes, stderr bytes and exit status, compared exactly).
//!
//! What is new here is the **census**. `go_cli_differential` can only assert
//! parity, so it can only carry cases that already agree; every surface the
//! Rust CLI has not reached yet is simply absent from it, and absence is
//! indistinguishable from a case nobody wrote. This gate instead runs *both*
//! binaries over the whole advertised command surface and classifies each row:
//!
//! * `parity` — stdout, stderr and exit status are byte-identical.
//! * `gap` — Go implements it (exit 0, output) and Rust refuses (non-zero
//!   exit, empty stdout). The refusal's exit code and first stderr line are
//!   pinned. **This is a recorded row, never a skip**: the gate proves Go does
//!   implement the surface, so the row is evidence of a Rust absence rather
//!   than of a shared one, and a gap that silently closes fails just as loudly
//!   as a parity that breaks.
//! * `rejected-both` — both refuse with empty stdout and different wording;
//!   both first lines are pinned.
//! * `divergence` — both implement it and disagree. Requires a `reason` file.
//!
//! A row whose observed classification differs from its frozen one fails the
//! gate **in either direction**. That is the assertion B5-DESIGN's DECISION-2
//! asks for — "a command present in one binary and absent in the other is a
//! failure, not a skip" — expressed so that the failure names which side is
//! missing.
//!
//! Every case runs against a private copied home under `env -i` with an
//! explicit allowlist, and the whole fixture tree is compared before and after:
//! these are no-effect invocations, so a case that writes anything fails.
//!
//! **The oracle is mandatory.** A missing or unusable Go binary is a hard
//! failure naming exactly what was checked and where — never an `eprintln!` +
//! `Ok(())` green (TRK-1280).

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

mod support;
use support::{frozen_go_oracle, frozen_tree_manifest};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------------------
// The case corpus
// ---------------------------------------------------------------------------

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cli-oracle/cases")
}

/// One frozen census row.
#[derive(Debug, Eq, PartialEq)]
enum Classification {
    Parity,
    Gap {
        rust_exit: i32,
        rust_first_line: String,
    },
    RejectedBoth {
        go_first_line: String,
        rust_first_line: String,
    },
    Divergence {
        go_first_line: String,
        rust_first_line: String,
    },
}

impl Classification {
    fn label(&self) -> &'static str {
        match self {
            Self::Parity => "parity",
            Self::Gap { .. } => "gap",
            Self::RejectedBoth { .. } => "rejected-both",
            Self::Divergence { .. } => "divergence",
        }
    }
}

struct Case {
    name: String,
    argv: Vec<String>,
    frozen: Classification,
    reason: Option<String>,
}

impl Case {
    /// Loads one case directory.
    ///
    /// A malformed or missing member is an `Err`, which fails the calling test
    /// — never a skip. `a_missing_case_is_a_hard_failure_rather_than_a_skip`
    /// checks that property directly.
    fn load(directory: &Path) -> TestResult<Self> {
        let name = directory
            .file_name()
            .ok_or("case directory has no name")?
            .to_string_lossy()
            .into_owned();
        let read = |leaf: &str| -> TestResult<String> {
            fs::read_to_string(directory.join(leaf)).map_err(|error| {
                format!(
                    "missing case member {}/{leaf}: {error}\n\
                     regenerate with: ORCHESTRATOR_ACCEPTED_GO_BIN=<frozen in-tree go binary> \
                     RUST_BIN=<target/debug/orchestrator> \
                     crates/orchestrator-cli/tests/fixtures/cli-oracle/regenerate.sh",
                    directory.display()
                )
                .into()
            })
        };
        let argv = read("argv")?
            .lines()
            .map(str::to_owned)
            .collect::<Vec<String>>();
        let class = read("class")?.trim().to_owned();
        let frozen = match class.as_str() {
            "parity" => Classification::Parity,
            "gap" => Classification::Gap {
                rust_exit: read("rust-exit")?.trim().parse()?,
                rust_first_line: read("rust-stderr")?.trim_end().to_owned(),
            },
            "rejected-both" => Classification::RejectedBoth {
                go_first_line: read("go-first-line")?.trim_end().to_owned(),
                rust_first_line: read("rust-first-line")?.trim_end().to_owned(),
            },
            "divergence" => Classification::Divergence {
                go_first_line: read("go-first-line")?.trim_end().to_owned(),
                rust_first_line: read("rust-first-line")?.trim_end().to_owned(),
            },
            other => return Err(format!("{name}: unknown frozen class {other:?}").into()),
        };
        let reason = fs::read_to_string(directory.join("reason"))
            .ok()
            .map(|text| text.trim().to_owned());
        Ok(Self {
            name,
            argv,
            frozen,
            reason,
        })
    }

    fn all() -> TestResult<Vec<Self>> {
        let root = corpus_root();
        let mut entries = fs::read_dir(&root)
            .map_err(|error| format!("cannot read the case corpus {}: {error}", root.display()))?
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        let cases = entries
            .into_iter()
            .filter(|entry| entry.path().is_dir())
            .map(|entry| Self::load(&entry.path()))
            .collect::<TestResult<Vec<Self>>>()?;
        if cases.is_empty() {
            return Err(format!("the case corpus {} is empty", root.display()).into());
        }
        Ok(cases)
    }
}

// ---------------------------------------------------------------------------
// The fixture home and the observation
// ---------------------------------------------------------------------------

#[derive(Debug, Eq, PartialEq)]
struct Observation {
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Observation {
    /// The first non-blank line of stdout-then-stderr, which is what the
    /// census pins for a row whose two sides do not agree byte-for-byte.
    fn first_line(&self) -> String {
        let stdout = String::from_utf8_lossy(&self.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&self.stderr).into_owned();
        stdout
            .lines()
            .chain(stderr.lines())
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .to_owned()
    }
}

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn create() -> io::Result<Self> {
        let root = fs::canonicalize(std::env::temp_dir())?.join(format!(
            "orchestrator-cli-oracle-matrix-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        for leaf in ["home", "config", "personas", "empty-path"] {
            fs::create_dir_all(root.join(leaf))?;
        }
        fs::write(root.join("home/sentinel"), b"unchanged\n")?;
        Ok(Self { root })
    }

    /// Runs one binary under `env -i` plus an explicit allowlist, following
    /// `tests/writer-authority-cross-language.sh`'s discipline: no ambient
    /// toolchain, wrapper, runner or configuration input reaches either side.
    fn observe(&self, binary: &Path, argv: &[String]) -> io::Result<Observation> {
        let output = Command::new(binary)
            .args(argv)
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("ORCHESTRATOR_CONFIG_DIR", self.root.join("config"))
            .env("ORCHESTRATOR_PERSONAS_DIR", self.root.join("personas"))
            .env("PATH", self.root.join("empty-path"))
            .env("TMPDIR", &self.root)
            .output()?;
        Ok(Observation {
            exit_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn image(&self) -> io::Result<BTreeMap<PathBuf, Option<Vec<u8>>>> {
        let mut image = BTreeMap::new();
        collect(&self.root, &self.root, &mut image)?;
        Ok(image)
    }
}

fn collect(
    root: &Path,
    directory: &Path,
    image: &mut BTreeMap<PathBuf, Option<Vec<u8>>>,
) -> io::Result<()> {
    let mut children = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        let path = child.path();
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        if child.file_type()?.is_dir() {
            image.insert(relative, None);
            collect(root, &path, image)?;
        } else {
            image.insert(relative, Some(fs::read(&path)?));
        }
    }
    Ok(())
}

/// Classifies one observed pair by the same rules `regenerate.sh` applies.
fn classify(go: &Observation, rust: &Observation) -> Classification {
    if go.exit_code == rust.exit_code && go.stdout == rust.stdout && go.stderr == rust.stderr {
        return Classification::Parity;
    }
    if go.exit_code == Some(0) && rust.stdout.is_empty() && rust.exit_code != Some(0) {
        return Classification::Gap {
            rust_exit: rust.exit_code.unwrap_or(-1),
            rust_first_line: rust.first_line(),
        };
    }
    if go.exit_code != Some(0)
        && rust.exit_code != Some(0)
        && go.stdout.is_empty()
        && rust.stdout.is_empty()
    {
        return Classification::RejectedBoth {
            go_first_line: go.first_line(),
            rust_first_line: rust.first_line(),
        };
    }
    Classification::Divergence {
        go_first_line: go.first_line(),
        rust_first_line: rust.first_line(),
    }
}

/// One case's two live observations: the oracle's, then the Rust CLI's.
type ObservedPair = (Observation, Observation);

/// One live observation of every case under both binaries.
fn observe_corpus() -> TestResult<(Vec<Case>, Vec<ObservedPair>)> {
    let go_binary = frozen_go_oracle()?;
    let rust_binary = PathBuf::from(env!("CARGO_BIN_EXE_orchestrator"));
    let cases = Case::all()?;
    let fixture = Fixture::create()?;
    let before = fixture.image()?;

    let mut observations = Vec::with_capacity(cases.len());
    for case in &cases {
        let go = fixture.observe(&go_binary, &case.argv)?;
        let rust = fixture.observe(&rust_binary, &case.argv)?;
        observations.push((go, rust));
    }

    assert_eq!(
        fixture.image()?,
        before,
        "a no-effect CLI probe mutated its fixture home"
    );
    Ok((cases, observations))
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
fn every_case_matches_its_frozen_classification() -> TestResult {
    let (cases, observations) = observe_corpus()?;
    let mut drift = Vec::new();
    for (case, (go, rust)) in cases.iter().zip(&observations) {
        let observed = classify(go, rust);
        if observed != case.frozen {
            drift.push(format!(
                "{} ({}): argv {:?}\n    frozen   {:?}\n    observed ({}) {:?}",
                case.name,
                case.frozen.label(),
                case.argv,
                case.frozen,
                observed.label(),
                observed
            ));
        }
    }
    assert!(
        drift.is_empty(),
        "the Go/Rust CLI census drifted in {} row(s) — a gap that closed is as \
         much a drift as a parity that broke, and both must be re-frozen \
         deliberately:\n{}",
        drift.len(),
        drift.join("\n")
    );
    Ok(())
}

#[test]
fn every_parity_row_is_byte_equal_under_both_binaries() -> TestResult {
    let (cases, observations) = observe_corpus()?;
    let mut parity = 0_usize;
    for (case, (go, rust)) in cases.iter().zip(&observations) {
        if case.frozen != Classification::Parity {
            continue;
        }
        parity += 1;
        assert_eq!(
            rust.exit_code, go.exit_code,
            "{} ({:?}) exit status",
            case.name, case.argv
        );
        assert_eq!(
            String::from_utf8_lossy(&rust.stdout),
            String::from_utf8_lossy(&go.stdout),
            "{} ({:?}) stdout",
            case.name,
            case.argv
        );
        assert_eq!(
            String::from_utf8_lossy(&rust.stderr),
            String::from_utf8_lossy(&go.stderr),
            "{} ({:?}) stderr",
            case.name,
            case.argv
        );
    }
    assert!(parity > 0, "the census carries no parity row at all");
    Ok(())
}

#[test]
fn every_gap_row_is_a_recorded_rust_absence_and_not_a_shared_one() -> TestResult {
    let (cases, observations) = observe_corpus()?;
    let mut gaps = 0_usize;
    for (case, (go, rust)) in cases.iter().zip(&observations) {
        let Classification::Gap {
            rust_exit,
            rust_first_line,
        } = &case.frozen
        else {
            continue;
        };
        gaps += 1;
        // The half that makes this a *gap row* rather than a skip: the oracle
        // implements the surface, so the row records which side is missing.
        assert_eq!(
            go.exit_code,
            Some(0),
            "{}: the oracle does not implement this surface, so the row is not a Rust gap",
            case.name
        );
        assert!(
            !go.stdout.is_empty(),
            "{}: the oracle produced no output, so the row is not a Rust gap",
            case.name
        );
        assert_eq!(
            rust.exit_code,
            Some(*rust_exit),
            "{}: the refusal's exit status moved",
            case.name
        );
        assert!(
            rust.stdout.is_empty(),
            "{}: a refusing command wrote to stdout",
            case.name
        );
        assert_eq!(
            &rust.first_line(),
            rust_first_line,
            "{}: the refusal's wording moved",
            case.name
        );
    }
    assert!(
        gaps > 0,
        "the census carries no gap row; if every surface is implemented the \
         corpus must say so explicitly rather than by omission"
    );
    Ok(())
}

#[test]
fn every_rejected_both_row_is_refused_by_both_binaries() -> TestResult {
    let (cases, observations) = observe_corpus()?;
    for (case, (go, rust)) in cases.iter().zip(&observations) {
        let Classification::RejectedBoth {
            go_first_line,
            rust_first_line,
        } = &case.frozen
        else {
            continue;
        };
        assert_ne!(
            go.exit_code,
            Some(0),
            "{}: the oracle accepted it",
            case.name
        );
        assert_ne!(
            rust.exit_code,
            Some(0),
            "{}: the Rust CLI accepted what the oracle rejects",
            case.name
        );
        assert!(
            go.stdout.is_empty() && rust.stdout.is_empty(),
            "{}",
            case.name
        );
        assert_eq!(&go.first_line(), go_first_line, "{}: Go wording", case.name);
        assert_eq!(
            &rust.first_line(),
            rust_first_line,
            "{}: Rust wording",
            case.name
        );
    }
    Ok(())
}

#[test]
fn every_divergence_row_carries_a_written_reason() -> TestResult {
    let cases = Case::all()?;
    for case in &cases {
        if !matches!(case.frozen, Classification::Divergence { .. }) {
            continue;
        }
        let reason = case
            .reason
            .as_deref()
            .ok_or_else(|| format!("{}: a divergence row needs a reason file", case.name))?;
        assert!(
            reason.len() > 40 && reason != "undocumented divergence",
            "{}: the divergence reason is a placeholder",
            case.name
        );
    }
    Ok(())
}

#[test]
fn every_row_that_carries_a_reason_is_a_divergence() -> TestResult {
    // A `reason` documents why a disagreement is recorded rather than fixed.
    // A row that stops disagreeing must not keep one, or the corpus asserts a
    // claim it no longer makes.
    for case in Case::all()? {
        if case.reason.is_some() {
            assert!(
                matches!(case.frozen, Classification::Divergence { .. }),
                "{} is {} but still carries a reason file",
                case.name,
                case.frozen.label()
            );
        }
    }
    Ok(())
}

#[test]
fn the_census_covers_every_command_the_oracle_advertises() -> TestResult {
    // DECISION-2's assertion, in the direction that catches drift *into* the
    // oracle: a command Go grows must appear in the corpus, or the census is
    // no longer a census. `evidence` is the row this would have caught — it is
    // absent from `is_go_command` and present in Go's help.
    let go_binary = frozen_go_oracle()?;
    let fixture = Fixture::create()?;
    let help = fixture.observe(&go_binary, &["--help".to_owned()])?;
    assert_eq!(help.exit_code, Some(0), "the oracle refused --help");
    let text = String::from_utf8(help.stdout)?;
    let advertised: Vec<String> = text
        .lines()
        .skip_while(|line| !line.starts_with("Available Commands:"))
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| line.split_whitespace().next())
        .filter(|command| *command != "help")
        .map(str::to_owned)
        .collect();
    assert!(
        advertised.len() >= 20,
        "parsed only {} advertised commands out of the oracle's help; the parse is wrong",
        advertised.len()
    );

    let covered: Vec<String> = Case::all()?.into_iter().map(|case| case.name).collect();
    let missing: Vec<&String> = advertised
        .iter()
        .filter(|command| !covered.contains(&format!("command-{command}-help")))
        .collect();
    assert!(
        missing.is_empty(),
        "the oracle advertises {missing:?}, which the census does not cover; \
         add a case directory rather than letting the surface drift silently"
    );
    Ok(())
}

#[test]
fn a_missing_case_is_a_hard_failure_rather_than_a_skip() {
    assert!(
        Case::load(&corpus_root().join("no-such-case")).is_err(),
        "a missing case directory did not fail the gate"
    );
    assert!(
        Case::load(&corpus_root().join("root-help")).is_ok(),
        "the loader rejects a case that exists"
    );
}

#[test]
fn a_missing_oracle_is_a_hard_failure_rather_than_a_skip() -> TestResult {
    // The resolver itself is the unit under test: `frozen_go_oracle` must
    // return an `Err` that names what was checked, and every case-running test
    // above propagates it with `?` so the gate goes red rather than green.
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
fn the_corpus_covers_the_flag_placements_the_design_names() -> TestResult {
    // Guards the corpus against quietly losing the cases B5-DESIGN §4 Gate 3
    // calls out by name: root/run help, persistent-flag placement on both
    // sides of the command word, an unknown command, and an unregistered flag.
    let names: Vec<String> = Case::all()?.into_iter().map(|case| case.name).collect();
    for required in [
        "root-help",
        "root-no-arguments",
        "root-unknown-command",
        "root-unknown-flag",
        "command-run-help",
        "flag-domain-before-run",
        "flag-domain-inline-after-run",
        "flag-help-word-domain-between",
        "command-status-unregistered-flag",
    ] {
        assert!(
            names.iter().any(|name| name == required),
            "the {required} case disappeared from the corpus"
        );
    }
    let unregistered = names
        .iter()
        .filter(|name| name.ends_with("-unregistered-flag"))
        .count();
    assert!(
        unregistered >= 20,
        "only {unregistered} commands are probed with --definitely-not-a-flag"
    );
    Ok(())
}
