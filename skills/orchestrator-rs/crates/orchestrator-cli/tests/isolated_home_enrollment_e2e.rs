//! B5-DESIGN §7's proving gate — the opt-in isolated-home door.
//!
//! `cargo test -p orchestrator-cli --locked --offline --test isolated_home_enrollment_e2e`
//!
//! Every case drives the **binary**, out of a bundle directory the test builds,
//! with `env_clear()` plus an explicit allowlist. Nothing here reaches a live
//! home, a live provider, a credential, or the network: the runtime home is a
//! fresh directory under the test's own fixture root beneath `TMPDIR`, the
//! provider is the attested helper (this test binary, invoked on a case that
//! returns immediately), and `NANIKA_LIVE_HOME_ENROLL` is never set — which
//! `the_live_home_selector_is_never_set` asserts rather than assumes.
//!
//! ## What the positive case claims
//!
//! [`i1_the_shipped_entrypoint_enrolls_through_the_isolated_door`] runs an
//! ordinary `orchestrator run` of an authored one-phase mission — no
//! `--dry-run`, no `--offline`, no canary path, no `FixtureEnrollment` — and
//! requires the durable artifacts of a real run: the canonical event log's
//! spawned/completed pair, the `missions` row `MetricsOwner` writes, and the
//! `checkpoint.json` carrying the authored plan.
//!
//! ## What the negative cases claim
//!
//! One case per row of §7.3's D1-D7. Each asserts the exact refusal **and**
//! that the target home is byte-identical before and after the attempt — a
//! door that refuses after writing has already failed, and a table whose rows
//! are only checked for their message would not notice.
//!
//! ## What this gate does *not* claim
//!
//! §7.4's case I2 — that the *installed* bundle's binary carries no
//! `test-support` in its feature resolution — is not here. This gate's bundle
//! is a copy of the binary `cargo test` builds, and that binary is unified with
//! this crate's `test-support` dev-dependency, so no case in this file can
//! witness the shipped feature set. That claim belongs to
//! `tests/production-distribution-contract.sh` over a real
//! `scripts/build-production-bundle.sh` bundle, and it is recorded as
//! outstanding rather than implied. What this gate *does* prove structurally is
//! that the door never touches the fixture half: it hands `seal` no
//! `FixtureEnrollment`, so the run it drives is the same `None` branch a
//! shipped binary takes.

#![allow(clippy::doc_markdown, reason = "prose names Go symbols and file paths")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use orchestrator_app::bundled_helper_digest;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static CASE: AtomicU64 = AtomicU64::new(1);

/// The label the manifest installs the helper under, and the file name it has
/// inside the bundle.
const HELPER: &str = "attested-helper";

/// The authored mission every case runs. One phase, `RUNTIME: codex` — the one
/// family the isolated door registers an executor for. The door widens the
/// home, never the provider, so a phase naming the live family would find no
/// executor and stay unenrolled (§7.3 D9).
const MISSION_SOURCE: &str = concat!(
    "# An authored one-phase mission\n",
    "\n",
    "PHASE: build | OBJECTIVE: run the attested helper | RUNTIME: codex\n",
);

// ---------------------------------------------------------------------------
// The bundle harness
// ---------------------------------------------------------------------------

/// One case's installed bundle, live-user home, checkout and target home.
///
/// Everything lives beneath a single directory under the process temporary
/// directory, which is what lets the unchanged `validate_policy_boundary`
/// admit the target home without the fixture policy being relaxed by one line.
struct Bundle {
    parent: PathBuf,
    bundle: PathBuf,
    home: PathBuf,
    live_user: PathBuf,
    checkout: PathBuf,
    personas: PathBuf,
    mission_file: PathBuf,
    temporary: PathBuf,
    helper_bytes: Vec<u8>,
}

impl Drop for Bundle {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

impl Bundle {
    fn new(label: &str) -> TestResult<Self> {
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let temporary = std::fs::canonicalize(std::env::temp_dir())?;
        let parent = temporary.join(format!(
            "orchestrator-rs-b5-isolated-{}-{number}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&parent);
        private_dir(&parent)?;

        let bundle = parent.join("bundle");
        let live_user = parent.join("live-user");
        let checkout = parent.join("checkout");
        let personas = parent.join("personas");
        for directory in [&bundle, &live_user, &checkout, &personas] {
            private_dir(directory)?;
        }

        // The installed binary. It is copied rather than symlinked, because the
        // door canonicalizes `argv[0]` before looking beside it: a symlink
        // would resolve back to `target/debug` and find no manifest there.
        std::fs::copy(
            env!("CARGO_BIN_EXE_orchestrator"),
            bundle.join("orchestrator"),
        )?;
        set_mode(&bundle.join("orchestrator"), 0o700)?;

        // The bundled helper is this test binary. `--exact
        // helper_quick_entrypoint` returns immediately and writes nothing, so
        // the phase reaches a terminal outcome without any side effect of its
        // own.
        let helper_bytes = std::fs::read(std::env::current_exe()?)?;
        std::fs::write(bundle.join(HELPER), &helper_bytes)?;
        set_mode(&bundle.join(HELPER), 0o700)?;

        let mission_file = parent.join("mission.md");
        std::fs::write(&mission_file, MISSION_SOURCE)?;

        let fixture = Self {
            home: parent.join("home"),
            parent,
            bundle,
            live_user,
            checkout,
            personas,
            mission_file,
            temporary,
            helper_bytes,
        };
        fixture.write_manifest(&bundled_helper_digest(&fixture.helper_bytes))?;
        Ok(fixture)
    }

    /// Writes the bundle manifest, with the caller's digest.
    ///
    /// The digest is a parameter so D7's case can pin a manifest that names a
    /// value the helper's bytes do not hash to.
    fn write_manifest(&self, helper_digest: &str) -> TestResult {
        std::fs::write(
            self.bundle.join("bundle-manifest.txt"),
            format!(
                "# B5-DESIGN §7.2 bundle manifest\n\
                 bundle_id=isolated-home-enrollment-e2e\n\
                 helper={HELPER}\n\
                 helper_sha256={helper_digest}\n\
                 helper_arg=--exact\n\
                 helper_arg=helper_quick_entrypoint\n\
                 helper_arg=--nocapture\n"
            ),
        )?;
        Ok(())
    }

    /// The argv a user types.
    fn argv(&self) -> Vec<String> {
        vec![
            "run".to_owned(),
            self.mission_file.to_string_lossy().into_owned(),
        ]
    }

    /// Runs the bundled binary with the default enrolled environment.
    fn run(&self) -> TestResult<Output> {
        self.run_with(|command| {
            command.env("NANIKA_ISOLATED_HOME_ENROLL", "1");
        })
    }

    /// Runs the bundled binary, letting the caller adjust the environment.
    ///
    /// `env_clear` first, always: it is what makes
    /// `the_live_home_selector_is_never_set` a fact about every case rather
    /// than a hope about the harness that runs them.
    fn run_with(&self, adjust: impl FnOnce(&mut Command)) -> TestResult<Output> {
        let mut command = Command::new(self.bundle.join("orchestrator"));
        command
            .args(self.argv())
            .current_dir(&self.checkout)
            .env_clear()
            .env("HOME", &self.live_user)
            .env("ALLUKA_HOME", &self.home)
            .env("TMPDIR", &self.temporary)
            .env("ORCHESTRATOR_PERSONAS_DIR", &self.personas)
            .env("PATH", self.parent.join("empty-path"));
        adjust(&mut command);
        Ok(command.output()?)
    }

    /// An ordered image of every byte under the target home, plus whether the
    /// home exists at all.
    fn home_image(&self) -> TestResult<BTreeMap<PathBuf, Option<Vec<u8>>>> {
        let mut image = BTreeMap::new();
        if self.home.exists() {
            image.insert(PathBuf::from("."), None);
            collect(&self.home, &self.home, &mut image)?;
        }
        Ok(image)
    }

    /// `(type, phase_id)` for every canonical event the run appended.
    fn events(&self) -> Vec<(String, String)> {
        let Ok(entries) = std::fs::read_dir(self.home.join("events")) else {
            return Vec::new();
        };
        let mut logs: Vec<PathBuf> = entries
            .filter_map(|entry| Some(entry.ok()?.path()))
            .collect();
        logs.sort();
        logs.iter()
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .flat_map(|text| {
                text.lines()
                    .filter(|line| !line.trim().is_empty())
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                    .map(|value| {
                        (
                            string_field(&value, "type"),
                            string_field(&value, "phase_id"),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Ordered `missions` rows, as `(id, status, phases_total, phases_completed)`.
    fn mission_rows(&self) -> TestResult<Vec<(String, String, i64, i64)>> {
        let database = self.home.join("metrics.db");
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

    /// The one workspace the run created, and its decoded checkpoint.
    fn checkpoint(&self) -> TestResult<orchestrator_core::CheckpointProjection> {
        let mut workspaces: Vec<PathBuf> = std::fs::read_dir(self.home.join("workspaces"))?
            .filter_map(|entry| Some(entry.ok()?.path()))
            .collect();
        workspaces.sort();
        let workspace = workspaces
            .first()
            .ok_or("the enrolled run created no workspace")?;
        let bytes = std::fs::read(workspace.join("checkpoint.json"))?;
        Ok(orchestrator_core::decode_checkpoint(&bytes)?.projection)
    }
}

fn collect(
    root: &Path,
    directory: &Path,
    image: &mut BTreeMap<PathBuf, Option<Vec<u8>>>,
) -> std::io::Result<()> {
    let mut children: Vec<PathBuf> = std::fs::read_dir(directory)?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .collect();
    children.sort();
    for path in children {
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        if path.symlink_metadata()?.is_dir() {
            image.insert(relative, None);
            collect(root, &path, image)?;
        } else {
            image.insert(relative, Some(std::fs::read(&path)?));
        }
    }
    Ok(())
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
    set_mode(path, 0o700)
}

fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

/// Asserts one refusal: the run failed and its stderr names `expected`.
///
/// The byte-identity half of each row is asserted by the caller, because what
/// counts as "the target home" differs per case — for D3 and D4 it is a path
/// the case names rather than the fixture's default one.
fn assert_refused(output: &Output, expected: &str) -> TestResult {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "the door admitted a run it must refuse: {stderr}"
    );
    assert!(
        stderr.contains(expected),
        "the refusal did not name {expected:?}: {stderr}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// I1 — the positive case
// ---------------------------------------------------------------------------

#[test]
fn i1_the_shipped_entrypoint_enrolls_through_the_isolated_door() -> TestResult {
    let bundle = Bundle::new("i1")?;
    assert!(
        !bundle.home.exists(),
        "the door must create the home it was pointed at"
    );

    let output = bundle.run()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the enrolled run failed: {stdout}{stderr}"
    );
    assert!(
        stdout.contains("execution: enrolled"),
        "an enrolled run that dispatched its phase must not report itself as \
         planning-only: {stdout}"
    );

    // The home is a real runtime home now, and it is exactly the one the
    // operator named.
    assert!(bundle.home.is_dir(), "the door created no home");

    // Events: the canonical log carries the phase's spawned/completed pair.
    assert_eq!(
        bundle.events(),
        vec![
            ("worker.spawned".to_owned(), "phase-1".to_owned()),
            ("worker.completed".to_owned(), "phase-1".to_owned()),
        ],
        "the enrolled run did not append the phase's canonical events"
    );

    // Metrics: `MetricsOwner` wrote the mission's terminal row.
    let rows = bundle.mission_rows()?;
    assert_eq!(rows.len(), 1, "expected exactly one mission row: {rows:?}");
    let (id, status, total, completed) = &rows[0];
    assert_eq!(status, "completed", "mission {id} ended {status}");
    // The aggregate counters are `count(*)` over the phase rows present when
    // `record_terminal` ran, and a phase row needs the `ExactProcessGroupAbsence`
    // witness `PhaseMetricIntent::new` demands — which the sealed run does not
    // mint for itself on either branch. `ordinary_authored_run_e2e`'s A2
    // observes the same `(0, 0)` through the fixture door, so this is the
    // enrolled shape, not a difference the isolated door introduces.
    assert_eq!(
        (*total, *completed),
        (0, 0),
        "mission {id} counted {rows:?}"
    );

    // Rows: the checkpoint carries the authored plan (CF-M4a-4), so a
    // rolled-back Go binary can read this home.
    let checkpoint = bundle.checkpoint()?;
    let plan = checkpoint
        .plan
        .as_ref()
        .ok_or("the enrolled run left a plan-less checkpoint")?;
    assert_eq!(plan.id, *id, "the plan is not this mission's");
    assert_eq!(
        plan.phases
            .iter()
            .map(|phase| (phase.id.as_str(), phase.name.as_str()))
            .collect::<Vec<_>>(),
        vec![("phase-1", "build")],
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// N1-N7 — one case per §7.3 row
// ---------------------------------------------------------------------------

/// D1 — the selector must be the exact string `"1"`.
///
/// Every other value leaves the run `Unenrolled` and `orchestrator run` exiting
/// `ExecutionNotEnrolled` exactly as it does today, and the home is never
/// created at all. That is also what keeps `run_not_enrolled` green unmodified.
#[test]
fn n1_a_selector_that_is_not_exactly_one_leaves_the_run_unenrolled() -> TestResult {
    let bundle = Bundle::new("n1")?;
    for value in [
        None,
        Some(""),
        Some("0"),
        Some("true"),
        Some("01"),
        Some("1 "),
    ] {
        let before = bundle.home_image()?;
        let output = bundle.run_with(|command| {
            if let Some(value) = value {
                command.env("NANIKA_ISOLATED_HOME_ENROLL", value);
            }
        })?;
        assert_refused(&output, "not enrolled")?;
        assert_eq!(
            bundle.home_image()?,
            before,
            "selector {value:?} changed the target home"
        );
        assert!(
            !bundle.home.exists(),
            "selector {value:?} created the target home"
        );
    }
    Ok(())
}

/// D2 — the home must have been named explicitly through `ALLUKA_HOME`.
///
/// Every other precedence rule reaches a home the operator did not choose for
/// this run. The case selects `ORCHESTRATOR_CONFIG_DIR`, which outranks
/// `ALLUKA_HOME`, so the resolved selection is not `AllukaHome` even though the
/// variable is set.
#[test]
fn n2_a_home_that_was_not_named_explicitly_is_refused() -> TestResult {
    let bundle = Bundle::new("n2")?;
    let config = bundle.parent.join("config-dir");
    private_dir(&config)?;
    let before = bundle.home_image()?;

    let output = bundle.run_with(|command| {
        command
            .env("NANIKA_ISOLATED_HOME_ENROLL", "1")
            .env("ORCHESTRATOR_CONFIG_DIR", &config);
    })?;
    assert_refused(&output, "requires an explicit ALLUKA_HOME")?;
    assert_eq!(bundle.home_image()?, before);
    assert!(!bundle.home.exists());
    Ok(())
}

/// D3 — the home may not overlap `$HOME/.alluka` or `$HOME/.via`.
///
/// Equality, containment and reverse containment all count, which is the same
/// relation the unchanged fixture policy's `reject_overlap` uses.
#[test]
fn n3_a_home_overlapping_a_live_home_is_refused() -> TestResult {
    let bundle = Bundle::new("n3")?;
    for leaf in [".alluka", ".via", ".alluka/nested"] {
        let live = bundle.live_user.join(leaf);
        let output = bundle.run_with(|command| {
            command
                .env("NANIKA_ISOLATED_HOME_ENROLL", "1")
                .env("ALLUKA_HOME", &live);
        })?;
        assert_refused(&output, "overlapping live-")?;
        assert!(
            !live.exists(),
            "the refused door created {}",
            live.display()
        );
    }
    Ok(())
}

/// D4 — the home may not be the repository checkout, or live inside it.
#[test]
fn n4_a_home_inside_the_repository_checkout_is_refused() -> TestResult {
    let bundle = Bundle::new("n4")?;
    for target in [bundle.checkout.clone(), bundle.checkout.join("home")] {
        let output = bundle.run_with(|command| {
            command
                .env("NANIKA_ISOLATED_HOME_ENROLL", "1")
                .env("ALLUKA_HOME", &target);
        })?;
        assert_refused(&output, "overlapping the repository checkout")?;
        assert!(
            !bundle.checkout.join("home").exists(),
            "the refused door created a home inside the checkout"
        );
    }
    Ok(())
}

/// D5 — the home must be an exact mode-0700, euid-owned directory.
///
/// The predicate is `is_private_fixture_directory`, reused verbatim from
/// fixture admission so the two cannot drift on what "private" means.
#[test]
fn n5_a_home_that_is_not_private_is_refused() -> TestResult {
    let bundle = Bundle::new("n5")?;
    std::fs::create_dir_all(&bundle.home)?;
    set_mode(&bundle.home, 0o755)?;
    let before = bundle.home_image()?;

    let output = bundle.run()?;
    assert_refused(&output, "not an exact mode-0700 owned directory")?;
    assert_eq!(bundle.home_image()?, before, "the refused door wrote to it");
    Ok(())
}

/// D6 — the home must be fresh, or already carry this binary's own marker.
///
/// A directory holding content this binary did not create is somebody else's,
/// and adopting it would mean writing a runtime home over it.
#[test]
fn n6_a_home_holding_foreign_content_is_refused() -> TestResult {
    let bundle = Bundle::new("n6")?;
    private_dir(&bundle.home)?;
    std::fs::write(bundle.home.join("somebody-elses-file"), b"do not touch\n")?;
    private_dir(&bundle.home.join("somebody-elses-directory"))?;
    let before = bundle.home_image()?;

    let output = bundle.run()?;
    assert_refused(&output, "holding content it did not create")?;
    assert_eq!(bundle.home_image()?, before, "the refused door wrote to it");
    Ok(())
}

/// D7 — the helper's bytes must be the ones the bundle manifest pinned.
///
/// Three shapes, because the interesting failure is not "the digests differ"
/// but "the environment variable was allowed to decide". The manifest is the
/// source of truth: a mistyped `NANIKA_BUNDLED_HELPER_SHA256` must *fail* the
/// check, never silence it, and a correct one cannot rescue a bad manifest.
#[test]
fn n7_a_helper_that_does_not_match_the_manifest_digest_is_refused() -> TestResult {
    let bundle = Bundle::new("n7")?;
    let real = bundled_helper_digest(&bundle.helper_bytes);
    let wrong = bundled_helper_digest(b"not the bundled helper");

    // (a) the manifest names a digest the helper does not hash to.
    bundle.write_manifest(&wrong)?;
    let before = bundle.home_image()?;
    let output = bundle.run()?;
    assert_refused(&output, "the bundled helper digest is")?;
    assert_eq!(bundle.home_image()?, before);
    assert!(!bundle.home.exists());

    // (b) the env cross-check disagrees with a *correct* manifest. It confirms
    //     or it refuses; it never overrides.
    bundle.write_manifest(&real)?;
    let output = bundle.run_with(|command| {
        command
            .env("NANIKA_ISOLATED_HOME_ENROLL", "1")
            .env("NANIKA_BUNDLED_HELPER_SHA256", &wrong);
    })?;
    assert_refused(&output, "the bundled helper digest is")?;
    assert!(!bundle.home.exists());

    // (c) a *correct* env cross-check cannot rescue a bad manifest — the env
    //     digest is never the source.
    bundle.write_manifest(&wrong)?;
    let output = bundle.run_with(|command| {
        command
            .env("NANIKA_ISOLATED_HOME_ENROLL", "1")
            .env("NANIKA_BUNDLED_HELPER_SHA256", &real);
    })?;
    assert_refused(&output, "the bundled helper digest is")?;
    assert!(
        !bundle.home.exists(),
        "an env-supplied digest silenced the manifest check"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The gate's own constraints
// ---------------------------------------------------------------------------

/// The §3.2 assertion, applied here: this gate never sets the live-home
/// selector, and it never reaches `$HOME/.alluka`.
///
/// Every case runs under `env_clear()`, so the variable cannot leak in from the
/// harness either. This case states that as a fact about the source rather than
/// leaving it to be inferred from each `run_with` call.
#[test]
fn the_live_home_selector_is_never_set() {
    let source = include_str!("isolated_home_enrollment_e2e.rs");
    let mentions: Vec<&str> = source
        .lines()
        .filter(|line| line.contains("NANIKA_LIVE_HOME_ENROLL"))
        .collect();
    assert_eq!(
        mentions.len(),
        3,
        "the live-home selector may appear only in this gate's own prose and \
         this assertion, observed {mentions:?}"
    );
    assert!(
        !source.contains(".env(\"NANIKA_LIVE_HOME_ENROLL\""),
        "no case may set the live-home selector"
    );
    assert!(
        source.contains(".env_clear()"),
        "every case must run under a cleared environment"
    );
}

/// The attested helper's entry point.
///
/// It returns immediately and writes nothing, so a released phase reaches a
/// terminal outcome without a side effect of its own.
#[test]
fn helper_quick_entrypoint() {}
