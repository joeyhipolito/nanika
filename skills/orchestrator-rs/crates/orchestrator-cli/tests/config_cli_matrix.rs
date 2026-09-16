//! B5-DESIGN §4, Gate 5 — home, config, runtime and model selection, compared
//! to the frozen in-tree Go oracle over synthetic paths and credentials.
//!
//! Adapted from `orchestrator-app/tests/home_precedence_table.rs`, which proves
//! the same five [`HomeSelection`] variants against a `DirectoryProbe` double.
//! That table can say what `RuntimeHomeResolver` decides; it cannot say whether
//! Go decides the same thing. This gate crosses the same five variants with
//! present, absent and malformed `config.yaml` and runs **both binaries** over
//! each synthetic home, comparing stdout, stderr and exit status byte-for-byte.
//!
//! Two properties make the comparison non-vacuous:
//!
//! * Every case seeds a *decoy* workspace into every candidate home the case
//!   does not expect to be selected, with a distinct workspace id. A precedence
//!   error therefore prints a different line rather than the same empty
//!   listing, so agreement is evidence and not coincidence.
//!   `the_decoys_discriminate` proves that directly.
//! * Nothing outside the fixture parent is reachable. Each leg runs under
//!   `env -i` plus an explicit allowlist (`tests/writer-authority-cross-language.sh`'s
//!   discipline), the synthetic Claude credentials file is inside the fixture and
//!   named by `CLAUDE_CREDENTIALS_FILE`, and every observed stream is checked
//!   for absolute paths that escape the fixture root.
//!
//! Runtime and model selection are compared through the dry-run plan, which is
//! the only place either binary renders a resolved phase. Go renders
//! `phase-N: name (persona, tier)`; Rust renders the same identity plus the
//! resolved runtime and model. The comparable projection is therefore
//! `(id, name, persona, tier)` per phase plus the phase count and execution
//! mode — named explicitly here rather than left implicit in a byte diff,
//! because Go's dry-run prints neither the runtime nor the model id.
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

/// The runtime family the authored mission pins. It is in the Go-compatible
/// supported set, so `orchestrator_core::resolve_runtime`'s clamp keeps it.
const AUTHORED_RUNTIME: &str = "codex";

// ---------------------------------------------------------------------------
// The oracle
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Synthetic homes
// ---------------------------------------------------------------------------

/// Which precedence rung a case expects to win, mirroring
/// `orchestrator_app::HomeSelection` one-for-one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Rung {
    OrchestratorConfigDir,
    AllukaHome,
    ViaHome,
    ExistingAlluka,
    ViaFallback,
}

impl Rung {
    const ALL: [Self; 5] = [
        Self::OrchestratorConfigDir,
        Self::AllukaHome,
        Self::ViaHome,
        Self::ExistingAlluka,
        Self::ViaFallback,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::OrchestratorConfigDir => "orchestrator-config-dir",
            Self::AllukaHome => "alluka-home",
            Self::ViaHome => "via-home",
            Self::ExistingAlluka => "existing-alluka",
            Self::ViaFallback => "via-fallback",
        }
    }
}

/// One disposable fixture: a private parent holding a synthetic user home,
/// every candidate runtime home, synthetic personas, an empty `PATH`, and a
/// synthetic Claude credentials file. Nothing it names lies outside `parent`.
struct Fixture {
    parent: PathBuf,
    rung: Rung,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

impl Fixture {
    fn new(rung: Rung, label: &str) -> TestResult<Self> {
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let parent = fs::canonicalize(std::env::temp_dir())?.join(format!(
            "orchestrator-rs-b5-config-{}-{number}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&parent);
        private_dir(&parent)?;

        let fixture = Self { parent, rung };
        fs::create_dir_all(fixture.user_home())?;
        fs::create_dir_all(fixture.empty_path())?;
        fixture.write_personas()?;
        // A synthetic credential, inside the fixture, named explicitly. The
        // shape is the one `internal/usage/probe.go` parses; the token is not a
        // secret and reaches no network because `PATH` is empty and no runtime
        // binary exists.
        fs::create_dir_all(fixture.user_home().join(".claude"))?;
        fs::write(
            fixture.credentials(),
            br#"{"claudeAiOauth":{"accessToken":"synthetic-fixture-token"}}"#,
        )?;

        // Every candidate home exists as a directory so the case differs only
        // in which one the *precedence rule* selects, and each non-selected one
        // carries a decoy workspace whose id names it.
        for rung in Rung::ALL {
            let home = fixture.home_of(rung);
            fs::create_dir_all(&home)?;
            let expected = rung == fixture.rung;
            seed_workspace(
                &home,
                if expected {
                    "ws-selected"
                } else {
                    decoy_id(rung)
                },
            )?;
        }
        Ok(fixture)
    }

    fn user_home(&self) -> PathBuf {
        self.parent.join("user")
    }

    fn empty_path(&self) -> PathBuf {
        self.parent.join("empty-path")
    }

    fn personas(&self) -> PathBuf {
        self.parent.join("personas")
    }

    fn credentials(&self) -> PathBuf {
        self.user_home().join(".claude/.credentials.json")
    }

    fn home_of(&self, rung: Rung) -> PathBuf {
        match rung {
            Rung::OrchestratorConfigDir => self.parent.join("explicit-config"),
            Rung::AllukaHome => self.parent.join("alluka"),
            Rung::ViaHome => self.parent.join("via/orchestrator"),
            Rung::ExistingAlluka => self.user_home().join(".alluka"),
            Rung::ViaFallback => self.user_home().join(".via"),
        }
    }

    /// The home this case's environment must actually select.
    fn selected_home(&self) -> PathBuf {
        self.home_of(self.rung)
    }

    fn write_personas(&self) -> io::Result<()> {
        fs::create_dir_all(self.personas())?;
        for (name, triggers) in [
            ("senior-backend-engineer", "build"),
            ("qa-engineer", "verify"),
        ] {
            fs::write(
                self.personas().join(format!("{name}.md")),
                format!(
                    "---\nrole: implementer\ncapabilities:\n  - {triggers}\ntriggers:\n  - {triggers}\n---\n\n# {name}\n\nSynthetic fixture persona.\n"
                ),
            )?;
        }
        Ok(())
    }

    /// The environment allowlist for this case's rung. Only the variables the
    /// selected rung needs are set, so precedence is exercised by *absence* the
    /// way the real resolver sees it.
    fn command(&self, binary: &Path, argv: &[&str]) -> Command {
        let mut command = Command::new(binary);
        command
            .args(argv)
            .current_dir(&self.parent)
            .env_clear()
            .env("HOME", self.user_home())
            .env("ORCHESTRATOR_PERSONAS_DIR", self.personas())
            .env("PATH", self.empty_path())
            .env("TMPDIR", &self.parent)
            .env("CLAUDE_CREDENTIALS_FILE", self.credentials());
        match self.rung {
            Rung::OrchestratorConfigDir => {
                command.env("ORCHESTRATOR_CONFIG_DIR", self.home_of(self.rung));
                command.env("ALLUKA_HOME", self.home_of(Rung::AllukaHome));
                command.env("VIA_HOME", self.parent.join("via"));
            }
            Rung::AllukaHome => {
                command.env("ALLUKA_HOME", self.home_of(self.rung));
                command.env("VIA_HOME", self.parent.join("via"));
            }
            Rung::ViaHome => {
                command.env("VIA_HOME", self.parent.join("via"));
            }
            // `ExistingAlluka` wins over `ViaFallback` only because
            // `$HOME/.alluka` exists, so the fallback case must remove it.
            Rung::ExistingAlluka | Rung::ViaFallback => {}
        }
        command
    }

    fn observe(&self, binary: &Path, argv: &[&str]) -> io::Result<Observation> {
        let output = self.command(binary, argv).output()?;
        Ok(Observation {
            exit_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Observation {
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Observation {
    fn text(&self) -> String {
        format!(
            "{}{}",
            String::from_utf8_lossy(&self.stdout),
            String::from_utf8_lossy(&self.stderr)
        )
    }
}

fn decoy_id(rung: Rung) -> &'static str {
    match rung {
        Rung::OrchestratorConfigDir => "ws-decoy-orchestrator-config-dir",
        Rung::AllukaHome => "ws-decoy-alluka-home",
        Rung::ViaHome => "ws-decoy-via-home",
        Rung::ExistingAlluka => "ws-decoy-existing-alluka",
        Rung::ViaFallback => "ws-decoy-via-fallback",
    }
}

fn private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Writes one terminal workspace both binaries can list.
///
/// The checkpoint is the envelope-v1/payload-v2 shape `core.LoadCheckpoint`
/// reads and `orchestrator_core::encode_current_checkpoint` writes.
fn seed_workspace(home: &Path, id: &str) -> io::Result<()> {
    let workspace = home.join("workspaces").join(id);
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("mission.md"), format!("mission for {id}\n"))?;
    fs::write(
        workspace.join("checkpoint.json"),
        format!(
            r#"{{"version":1,"payload":{{"version":2,"workspace_id":"{id}","domain":"dev","plan":{{"id":"plan-{id}","task":"fixture task","phases":[{{"id":"phase-1","name":"build","status":"completed"}}]}},"status":"completed","started_at":"2026-07-13T00:00:00Z"}}}}"#
        ),
    )
}

fn authored_mission() -> String {
    format!(
        "PHASE: build | OBJECTIVE: compile the crate | PERSONA: senior-backend-engineer | RUNTIME: {AUTHORED_RUNTIME}\n\
         PHASE: verify | OBJECTIVE: run the gates | PERSONA: qa-engineer | RUNTIME: {AUTHORED_RUNTIME} | DEPENDS: build\n"
    )
}

// ---------------------------------------------------------------------------
// The dry-run plan projection
// ---------------------------------------------------------------------------

/// The facts both binaries render for a resolved phase.
///
/// Go prints `  phase-1: build (senior-backend-engineer, work)`; Rust prints
/// `  - phase-1 build [persona=… role=… tier=… runtime=… model=… effort=…]`.
/// The intersection is the identity plus the persona and the model tier, which
/// is exactly the "path/runtime/model selection" this gate can compare across
/// the boundary: Go's dry-run renders the tier but neither the runtime nor the
/// resolved model id.
#[derive(Debug, Eq, PartialEq)]
struct PhaseProjection {
    id: String,
    name: String,
    persona: String,
    tier: String,
}

#[derive(Debug, Eq, PartialEq)]
struct PlanProjection {
    phases: Vec<PhaseProjection>,
    sequential: bool,
}

fn project_go_plan(text: &str) -> TestResult<PlanProjection> {
    let mut phases = Vec::new();
    let mut sequential = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("plan: ") {
            sequential = rest.contains("(sequential)");
            continue;
        }
        let Some((identity, tail)) = trimmed.split_once(": ") else {
            continue;
        };
        if !identity.starts_with("phase-") {
            continue;
        }
        let (name, tail) = tail
            .split_once(" (")
            .ok_or_else(|| format!("unparsable Go plan line {line:?}"))?;
        let attributes = tail
            .split_once(')')
            .ok_or_else(|| format!("unparsable Go plan line {line:?}"))?
            .0;
        let (persona, tier) = attributes
            .split_once(", ")
            .ok_or_else(|| format!("unparsable Go plan attributes {line:?}"))?;
        phases.push(PhaseProjection {
            id: identity.to_owned(),
            name: name.to_owned(),
            persona: persona.to_owned(),
            tier: tier.to_owned(),
        });
    }
    if phases.is_empty() {
        return Err(format!("the Go dry-run rendered no phase line:\n{text}").into());
    }
    Ok(PlanProjection { phases, sequential })
}

fn project_rust_plan(text: &str) -> TestResult<PlanProjection> {
    let mut phases = Vec::new();
    let mut sequential = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("phases: ") {
            sequential = rest.contains("(sequential)");
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("- phase-") else {
            continue;
        };
        let (index, tail) = rest
            .split_once(' ')
            .ok_or_else(|| format!("unparsable Rust plan line {line:?}"))?;
        let (name, attributes) = tail
            .split_once(" [")
            .ok_or_else(|| format!("unparsable Rust plan line {line:?}"))?;
        let attributes = attributes.trim_end_matches(']');
        let field = |key: &str| -> TestResult<String> {
            attributes
                .split_whitespace()
                .find_map(|pair| pair.strip_prefix(&format!("{key}=")))
                .map(str::to_owned)
                .ok_or_else(|| format!("the Rust plan line {line:?} has no {key}").into())
        };
        phases.push(PhaseProjection {
            id: format!("phase-{index}"),
            name: name.to_owned(),
            persona: field("persona")?,
            tier: field("tier")?,
        });
    }
    if phases.is_empty() {
        return Err(format!("the Rust dry-run rendered no phase line:\n{text}").into());
    }
    Ok(PlanProjection { phases, sequential })
}

/// The runtime and model the Rust side resolved, which Go's dry-run does not
/// print. Asserted against the authored value rather than across the boundary.
fn rust_runtimes_and_models(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("- phase-").map(str::to_owned))
        .filter_map(|line| {
            let runtime = line
                .split_whitespace()
                .find_map(|pair| pair.strip_prefix("runtime="))?
                .trim_end_matches(']')
                .to_owned();
            let model = line
                .split_whitespace()
                .find_map(|pair| pair.strip_prefix("model="))?
                .trim_end_matches(']')
                .to_owned();
            Some((runtime, model))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

fn rust_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_orchestrator"))
}

/// Prepares one rung's fixture, including the deletions a lower rung needs.
fn fixture_for(rung: Rung, label: &str) -> TestResult<Fixture> {
    let fixture = Fixture::new(rung, label)?;
    if rung == Rung::ViaFallback {
        // `$HOME/.alluka` existing is exactly what makes `ExistingAlluka` win,
        // so the fallback case removes it — decoy and all.
        fs::remove_dir_all(fixture.home_of(Rung::ExistingAlluka))?;
    }
    Ok(fixture)
}

#[test]
fn every_home_precedence_rung_selects_the_same_home_in_both_binaries() -> TestResult {
    let go_binary = frozen_go_oracle()?;
    for rung in Rung::ALL {
        let fixture = fixture_for(rung, rung.label())?;
        let go = fixture.observe(&go_binary, &["status"])?;
        let rust = fixture.observe(&rust_binary(), &["status"])?;
        assert_eq!(
            rust.exit_code,
            go.exit_code,
            "{}: exit status",
            rung.label()
        );
        assert_eq!(
            String::from_utf8_lossy(&rust.stdout),
            String::from_utf8_lossy(&go.stdout),
            "{}: stdout",
            rung.label()
        );
        assert_eq!(
            String::from_utf8_lossy(&rust.stderr),
            String::from_utf8_lossy(&go.stderr),
            "{}: stderr",
            rung.label()
        );
        assert!(
            go.text().contains("ws-selected"),
            "{}: the oracle did not select {} — it listed {:?}",
            rung.label(),
            fixture.selected_home().display(),
            go.text()
        );
    }
    Ok(())
}

#[test]
fn the_decoys_discriminate() -> TestResult {
    // Anti-vacuity control for the case above: every non-selected candidate
    // home holds a workspace with a distinct id, so a precedence error would
    // print a decoy id instead of `ws-selected`. If it could not, agreement
    // between the two binaries would prove nothing.
    let go_binary = frozen_go_oracle()?;
    for rung in Rung::ALL {
        let fixture = fixture_for(rung, "decoy")?;
        for observed in [
            fixture.observe(&go_binary, &["status"])?,
            fixture.observe(&rust_binary(), &["status"])?,
        ] {
            let text = observed.text();
            for other in Rung::ALL {
                if other == rung {
                    continue;
                }
                assert!(
                    !text.contains(decoy_id(other)),
                    "{}: a decoy from {} leaked into the listing:\n{text}",
                    rung.label(),
                    other.label()
                );
            }
        }
        // And the decoys really are on disk, so their absence above is a
        // selection fact rather than a missing fixture.
        for other in Rung::ALL {
            if other == rung || (rung == Rung::ViaFallback && other == Rung::ExistingAlluka) {
                continue;
            }
            assert!(
                fixture
                    .home_of(other)
                    .join("workspaces")
                    .join(decoy_id(other))
                    .join("checkpoint.json")
                    .is_file(),
                "{}: the {} decoy was never written",
                rung.label(),
                other.label()
            );
        }
    }
    Ok(())
}

#[test]
fn an_empty_environment_variable_does_not_select_a_home() -> TestResult {
    // `optional_path`'s `filter(|value| !value.is_empty())`, cross-checked
    // against Go: an exported-but-empty override must fall through to the next
    // rung in both, so the `ExistingAlluka` fixture keeps selecting
    // `$HOME/.alluka` with every override present but empty.
    let go_binary = frozen_go_oracle()?;
    let fixture = fixture_for(Rung::ExistingAlluka, "empty-env")?;
    for binary in [&go_binary, &rust_binary()] {
        let output = fixture
            .command(binary, &["status"])
            .env("ORCHESTRATOR_CONFIG_DIR", "")
            .env("ALLUKA_HOME", "")
            .env("VIA_HOME", "")
            .output()?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.status.code(), Some(0), "{}", binary.display());
        assert!(
            text.contains("ws-selected"),
            "{} let an empty override select a home: {text}",
            binary.display()
        );
    }
    Ok(())
}

/// `config.yaml` in the selected home, in each of the three states the gate
/// crosses the precedence rungs with.
#[derive(Clone, Copy, Debug)]
enum ConfigState {
    Absent,
    Present,
    Malformed,
}

impl ConfigState {
    const ALL: [Self; 3] = [Self::Absent, Self::Present, Self::Malformed];

    fn label(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Present => "present",
            Self::Malformed => "malformed",
        }
    }

    fn write(self, home: &Path) -> io::Result<()> {
        match self {
            Self::Absent => Ok(()),
            Self::Present => fs::write(
                home.join("config.yaml"),
                "model_tiers:\n  work:\n    provider: fixture-provider\n    model: fixture-model\n    runtime: codex\n",
            ),
            // Not YAML at all, so `serde_saphyr::from_str` fails and the loader
            // must degrade to an empty routing map rather than refuse the run.
            Self::Malformed => fs::write(home.join("config.yaml"), "model_tiers: [ unterminated\n"),
        }
    }
}

#[test]
fn every_rung_and_config_state_resolves_the_same_plan_in_both_binaries() -> TestResult {
    let go_binary = frozen_go_oracle()?;
    let rust = rust_binary();
    for rung in Rung::ALL {
        for state in ConfigState::ALL {
            let label = format!("{}-{}", rung.label(), state.label());
            let fixture = fixture_for(rung, &label)?;
            state.write(&fixture.selected_home())?;
            let mission = fixture.parent.join("mission.md");
            fs::write(&mission, authored_mission())?;
            let mission = mission.to_string_lossy().into_owned();

            let go = fixture.observe(&go_binary, &["run", &mission, "--dry-run"])?;
            let rust_observed = fixture.observe(&rust, &["run", &mission, "--dry-run"])?;
            assert_eq!(
                go.exit_code,
                Some(0),
                "{label}: the oracle refused: {}",
                go.text()
            );
            assert_eq!(
                rust_observed.exit_code,
                Some(0),
                "{label}: the Rust CLI refused: {}",
                rust_observed.text()
            );

            let go_plan = project_go_plan(&String::from_utf8_lossy(&go.stdout))?;
            let rust_plan = project_rust_plan(&String::from_utf8_lossy(&rust_observed.stdout))?;
            assert_eq!(
                rust_plan, go_plan,
                "{label}: the resolved plan projection diverged"
            );
            assert_eq!(go_plan.phases.len(), 2, "{label}");
            assert!(go_plan.sequential, "{label}");

            // The half Go does not print: the authored `RUNTIME:` survives
            // resolution on the Rust side and a model is selected for it.
            for (runtime, model) in
                rust_runtimes_and_models(&String::from_utf8_lossy(&rust_observed.stdout))
            {
                assert_eq!(runtime, AUTHORED_RUNTIME, "{label}: resolved runtime");
                assert!(!model.is_empty(), "{label}: no model was selected");
            }
        }
    }
    Ok(())
}

#[test]
fn a_malformed_config_degrades_identically_in_both_binaries() -> TestResult {
    // B5-DESIGN §4 Gate 5's negative assertion. A `config.yaml` that is not
    // YAML must not fail the run in either binary: both fall back to an empty
    // routing map, and the rendered plan is the one the absent-config case
    // produced.
    let go_binary = frozen_go_oracle()?;
    let rust = rust_binary();
    let mut projections = Vec::new();
    for state in [ConfigState::Absent, ConfigState::Malformed] {
        let fixture = fixture_for(Rung::AllukaHome, state.label())?;
        state.write(&fixture.selected_home())?;
        let mission = fixture.parent.join("mission.md");
        fs::write(&mission, authored_mission())?;
        let mission = mission.to_string_lossy().into_owned();
        let go = fixture.observe(&go_binary, &["run", &mission, "--dry-run"])?;
        let rust_observed = fixture.observe(&rust, &["run", &mission, "--dry-run"])?;
        assert_eq!(go.exit_code, rust_observed.exit_code, "{}", state.label());
        assert_eq!(go.exit_code, Some(0), "{}", state.label());
        projections.push(project_go_plan(&String::from_utf8_lossy(&go.stdout))?);
        projections.push(project_rust_plan(&String::from_utf8_lossy(
            &rust_observed.stdout,
        ))?);
    }
    let first = projections.first().ok_or("no projection")?;
    for projection in &projections {
        assert_eq!(
            projection, first,
            "a malformed config.yaml changed the resolved plan"
        );
    }
    Ok(())
}

#[test]
fn no_observed_path_escapes_the_fixture_root() -> TestResult {
    // "No credential opened outside its fixture", as an observable: each leg
    // runs under `env -i` with a synthetic `CLAUDE_CREDENTIALS_FILE` inside the
    // fixture, and no absolute path outside the fixture parent may appear in
    // either stream. A binary that fell back to `$HOME/.claude` on the real
    // user, or to an installed plugin directory, would name it here.
    let go_binary = frozen_go_oracle()?;
    let rust = rust_binary();
    for rung in Rung::ALL {
        let fixture = fixture_for(rung, "confinement")?;
        let mission = fixture.parent.join("mission.md");
        fs::write(&mission, authored_mission())?;
        let mission = mission.to_string_lossy().into_owned();
        let parent = fixture.parent.to_string_lossy().into_owned();
        let mut fixture_paths_seen = 0_usize;
        for (side, binary) in [("go", &go_binary), ("rust", &rust)] {
            for argv in [vec!["status"], vec!["run", mission.as_str(), "--dry-run"]] {
                let text = fixture.observe(binary, &argv)?.text();
                assert!(
                    !text.trim().is_empty(),
                    "{side} {argv:?} produced nothing to scan"
                );
                for token in text.split(|character: char| {
                    character.is_whitespace() || "\"'(),;:".contains(character)
                }) {
                    if !token.starts_with('/') {
                        continue;
                    }
                    fixture_paths_seen += 1;
                    // The system paths a Go runtime error would name are not
                    // fixture paths, and neither is a live `~/.alluka`.
                    assert!(
                        token.starts_with(&parent),
                        "{side} {argv:?} named {token}, which is outside the fixture root {parent}"
                    );
                }
            }
        }
        assert!(
            fixture_paths_seen > 0,
            "{}: no absolute path was observed at all, so the confinement scan \
             proved nothing",
            rung.label()
        );
        assert!(
            fixture.credentials().starts_with(&fixture.parent),
            "the synthetic credential escaped the fixture"
        );
    }
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
