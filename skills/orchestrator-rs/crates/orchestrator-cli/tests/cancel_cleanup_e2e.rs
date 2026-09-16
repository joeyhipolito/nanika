#![cfg(unix)]
//! B5-DESIGN §4, Gate 4 — cancellation and cleanup: a real fixture worker
//! terminated through TERM and its KILL escalation, leaving no descendants, and
//! a terminal cancel that is idempotent.
//!
//! Adapted from B4's trash-confinement work ([`TrashRoot`]) and from
//! `orchestrator-app/tests/owned_process_supervision.rs`, which already
//! establishes the shape: run the supervised fixture on a scoped thread, cancel
//! it from the parent, and prove the dedicated process group is gone with a
//! non-signalling `rustix` probe.
//!
//! Three things are new here.
//!
//! **The descendant that ignores `SIGTERM`.** The supervisor signals the whole
//! dedicated process group and escalates to `SIGKILL` after `term_grace`. A
//! case whose members all die on `SIGTERM` cannot tell the two legs apart. This
//! gate's tree entrypoint therefore spawns `/bin/sh -c 'trap "" TERM; exec sleep
//! …'` as a group member: the Rust leader dies on the default `SIGTERM`
//! disposition, the shell survives it, and the group can only become absent
//! because the `SIGKILL` escalation actually ran. Group absence is therefore
//! evidence of both legs, and "no descendants" is a measured fact rather than a
//! restatement of the leader's exit.
//!
//! **The attested helper is this test binary.** `is_native_executable` accepts
//! only a thin Mach-O or ELF image, and every macOS system binary is a fat
//! `cafebabe` image, so no system binary can be an attested fixture helper. The
//! helper is `std::env::current_exe()` re-executed with `--exact <entrypoint>`;
//! each entrypoint is inert unless `ORCHESTRATOR_FIXTURE_PROTOCOL=1`, which
//! `FixtureProcessAuthority` sets for supervised fixture processes and nothing
//! else does.
//!
//! **The oracle owns the terminal-cancel claim.** `compatibility/contracts.yaml`
//! `ORC-CLI-OPS-001` says cancel "is idempotent for terminal missions"; that is
//! a claim about the Go CLI, so it is asserted against the Go CLI, twice over
//! the same completed workspace, with the checkpoint and a pre-existing
//! operator file compared before and after. The Rust `cancel` surface does not
//! exist yet and is recorded as a **gap row**, never skipped.
//!
//! **The oracle is mandatory.** A missing or unusable Go binary is a hard
//! failure naming exactly what was checked and where — never an `eprintln!` +
//! `Ok(())` green (TRK-1280).

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use orchestrator_app::{
    CancellationToken, FixtureAdmissionPolicy, FixtureProcessAuthority, FixtureProcessReport,
    FixtureProcessSpec, FixtureWorkspaceSeed, FreshFixtureAuthority, GitEffectCapability,
    IsolatedFixtureRoot, ProcessTermination, SupervisorError, SupervisorLimits, TrashRoot,
};
use orchestrator_core::{CheckpointProjection, MissionId};
use rustix::process::{Pid, test_kill_process, test_kill_process_group};

mod support;
use support::{frozen_go_oracle, frozen_tree_manifest};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static CASE: AtomicU64 = AtomicU64::new(1);

/// The variable `FixtureProcessAuthority` puts in every supervised fixture
/// process's environment (`supervision.rs`'s environment allowlist). It is the
/// re-exec entrypoints' inertness gate: an ordinary `cargo test` run does not
/// set it, so they return immediately.
const PROTOCOL: &str = "ORCHESTRATOR_FIXTURE_PROTOCOL";

/// Long enough that nothing finishes on its own before the parent cancels.
const HELPER_LIFETIME_SECONDS: u64 = 30;

// ---------------------------------------------------------------------------
// Re-exec entrypoints
// ---------------------------------------------------------------------------

fn supervised() -> bool {
    std::env::var(PROTOCOL).as_deref() == Ok("1")
}

/// A fixture worker that outlives the test unless it is cancelled.
///
/// It publishes [`SLEEP_MARKER`] before sleeping for the reason the
/// [`DESCENDANT_MARKER`] comment below gives: ownership is true before this
/// image has exec'd, so a parent that cancels on ownership alone can cancel a
/// run that never spawned (B5-DESIGN §9.M).
#[test]
fn cancel_fixture_sleep_entrypoint() {
    if !supervised() {
        return;
    }
    if let Err(error) = announce_running(SLEEP_MARKER) {
        eprintln!("cannot publish the fixture marker: {error}");
        std::process::exit(64);
    }
    std::thread::sleep(Duration::from_secs(HELPER_LIFETIME_SECONDS));
    std::process::exit(0);
}

/// The file [`cancel_fixture_sleep_entrypoint`] publishes once it is really
/// running, in the same `TMPDIR` the descendant marker uses.
const SLEEP_MARKER: &str = "sleep.pid";

/// Publishes a readiness marker into the supervised `TMPDIR`.
///
/// Staged and then moved, so the parent's `is_file` poll never observes a
/// partial write — the same shape as [`announce_descendant`].
fn announce_running(name: &str) -> TestResult {
    let temporary = PathBuf::from(std::env::var("TMPDIR")?);
    fs::create_dir_all(&temporary)?;
    let staging = temporary.join(format!("{name}.partial"));
    fs::write(&staging, format!("{}\n", std::process::id()))?;
    fs::rename(&staging, temporary.join(name))?;
    Ok(())
}

/// The file the descendant entrypoint publishes into its own `TMPDIR`, which
/// `FixtureProcessAuthority` binds to `<root>/tmp`. The parent polls for it
/// rather than for mere supervisor ownership: ownership is true the instant
/// the leader is spawned, which is before it has exec'd, loaded and forked, so
/// cancelling on ownership alone would kill a group that has no descendant yet
/// and the escalation claim would be vacuous.
const DESCENDANT_MARKER: &str = "descendant.pid";

/// A fixture worker with one descendant that ignores `SIGTERM`.
///
/// The descendant inherits the dedicated process group (nothing calls
/// `setpgid`), so it is signalled with the leader. `trap "" TERM` makes it
/// survive the `SIGTERM` leg, and `exec sleep` replaces the shell so the
/// surviving member is a plain sleeping process rather than a shell waiting on
/// a child. Only the `SIGKILL` escalation can remove it.
#[test]
fn cancel_fixture_descendant_entrypoint() {
    if !supervised() {
        return;
    }
    if let Err(error) = announce_descendant() {
        eprintln!("cannot publish the descendant: {error}");
        std::process::exit(64);
    }
    std::thread::sleep(Duration::from_secs(HELPER_LIFETIME_SECONDS));
    std::process::exit(0);
}

/// Spawns the descendant and lets *it* publish the readiness marker.
///
/// The marker is written by the shell itself, after `trap` has installed the
/// ignored disposition and with `$$` as its content, so its presence proves
/// three things at once: the descendant exists, it already ignores `SIGTERM`,
/// and the pid the parent will probe is the one `exec` carries forward. A
/// marker written by *this* process right after `spawn` would race the shell's
/// startup and could be observed before the trap ran, which is exactly the way
/// this case could pass while proving nothing.
fn announce_descendant() -> TestResult {
    let temporary = PathBuf::from(std::env::var("TMPDIR")?);
    fs::create_dir_all(&temporary)?;
    let marker = temporary.join(DESCENDANT_MARKER);
    let staging = temporary.join("descendant.pid.partial");
    let script = format!(
        // `/bin/sleep` by absolute path: the supervised `PATH` is `<root>/bin`,
        // which holds only the attested helper, so a bare `sleep` would not
        // resolve and the shell would exit at once. An ignored disposition
        // survives `execve`, so the replacement image still ignores `SIGTERM`.
        r#"trap "" TERM; printf '%s\n' "$$" > '{staging}'; /bin/mv '{staging}' '{marker}'; exec /bin/sleep {HELPER_LIFETIME_SECONDS}"#,
        staging = staging.display(),
        marker = marker.display(),
    );
    Command::new("/bin/sh").args(["-c", &script]).spawn()?;
    Ok(())
}

/// A fixture worker that reaches a terminal outcome on its own.
#[test]
fn cancel_fixture_terminal_entrypoint() {
    if !supervised() {
        return;
    }
    println!("TERMINAL");
    std::process::exit(0);
}

// ---------------------------------------------------------------------------
// The oracle
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The supervised fixture
// ---------------------------------------------------------------------------

/// One disposable fixture: an isolated root, an authority admitted over it, a
/// workspace, and the process authority bound to this test binary as the
/// attested helper.
struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    authority: Option<FixtureProcessAuthority>,
    /// Minted from the freshly created handle before `admit` consumed it, so a
    /// `TrashRoot` can be named beneath it. `GitEffectCapability::in_fixture`
    /// refuses an adopted root.
    git_capability: GitEffectCapability,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.authority.take();
        let _ = fs::remove_dir_all(&self.parent);
    }
}

impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        // The `SIGTERM` leg must be observable, so `term_grace` is long enough
        // for a plain child to die on it and short enough that the escalation
        // still runs well inside a test.
        Self::with_limits(
            label,
            SupervisorLimits::for_tests(Duration::from_millis(250), Duration::from_millis(2_000))?,
        )
    }

    fn with_limits(label: &str, limits: SupervisorLimits) -> TestResult<Self> {
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let temporary = fs::canonicalize(std::env::temp_dir())?;
        let parent = temporary.join(format!(
            "orchestrator-rs-b5-cancel-{}-{number}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&parent);
        private_dir(&parent)?;

        let helper = fs::read(std::env::current_exe()?)?;
        let checkout = fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
        let policy = FixtureAdmissionPolicy::new(parent.join("live-user"), checkout, &temporary)
            .with_expected_fixture_helper(&helper);

        let fresh = IsolatedFixtureRoot::create_fresh(&parent)?;
        let root = fresh.path().to_path_buf();
        let git_capability = GitEffectCapability::in_fixture(&fresh, &["origin"])?;
        let authority = FreshFixtureAuthority::admit(fresh, &policy)?;

        let checkpoint = CheckpointProjection {
            workspace_id: format!("cancel-{number}"),
            status: "pending".to_owned(),
            started_at: "2026-07-13T00:00:00Z".to_owned(),
            ..CheckpointProjection::default()
        };
        let workspace = authority.create_workspace(
            MissionId::new(format!("cancel-{number}"))?,
            FixtureWorkspaceSeed::new(b"fixture\n".to_vec(), &checkpoint, b"{}".to_vec())?,
        )?;
        let executable = authority.install_fixture_executable("cancel-helper", &helper)?;
        let process = FixtureProcessAuthority::new(executable, &workspace, limits)?;

        Ok(Self {
            parent,
            root,
            authority: Some(process),
            git_capability,
        })
    }

    fn authority(&self) -> TestResult<&FixtureProcessAuthority> {
        self.authority
            .as_ref()
            .ok_or_else(|| "the fixture process authority was taken".into())
    }
}

fn private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Selects one re-exec entrypoint by libtest filter, exactly as the phase-2
/// composition gates do.
fn helper_spec(entrypoint: &str) -> TestResult<FixtureProcessSpec> {
    Ok(FixtureProcessSpec::new(
        ["--exact", entrypoint, "--nocapture"],
        Duration::from_secs(HELPER_LIFETIME_SECONDS + 30),
    )?)
}

/// Blocks until the supervised worker publishes its readiness marker.
fn wait_for_marker(path: &Path) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.is_file() {
        if Instant::now() >= deadline {
            return Err(format!("the supervised worker never published {}", path.display()).into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn wait_until_owned(authority: &FixtureProcessAuthority) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !authority.has_unresolved_processes() {
        if Instant::now() >= deadline {
            return Err("the production supervisor never registered the fixture process".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

fn join_report(
    result: std::thread::Result<Result<FixtureProcessReport, SupervisorError>>,
) -> TestResult<FixtureProcessReport> {
    let result = result.map_err(|_| io::Error::other("the fixture process thread panicked"))?;
    Ok(result?)
}

/// Non-signalling proof that a dedicated process group is gone.
fn assert_group_absent(pgid: Option<u32>) -> TestResult {
    let raw = pgid.ok_or("the report omitted its process-group ID")?;
    let pid = Pid::from_raw(i32::try_from(raw)?).ok_or("invalid pgid")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match test_kill_process_group(pid) {
            Err(rustix::io::Errno::SRCH) => return Ok(()),
            Ok(()) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(()) => return Err(format!("process group {raw} survived cancellation").into()),
            Err(error) => return Err(format!("cannot probe group {raw}: {error}").into()),
        }
    }
}

fn assert_pid_absent(raw: u32) -> TestResult {
    let pid = Pid::from_raw(i32::try_from(raw)?).ok_or("invalid pid")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match test_kill_process(pid) {
            Err(rustix::io::Errno::SRCH) => return Ok(()),
            Ok(()) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(()) => return Err(format!("process {raw} survived cancellation").into()),
            Err(error) => return Err(format!("cannot probe process {raw}: {error}").into()),
        }
    }
}

/// Runs one supervised entrypoint and cancels it once the supervisor owns it.
///
/// Only for entrypoints that are already in their final state the instant they
/// are spawned. `cancel_removes_a_descendant_that_ignores_sigterm` cannot use
/// this: it must additionally wait for its descendant's readiness marker, or
/// the cancel races the descendant's startup.
fn run_and_cancel(fixture: &Fixture, entrypoint: &str) -> TestResult<FixtureProcessReport> {
    let authority = fixture.authority()?;
    let token = CancellationToken::new();
    let spec = helper_spec(entrypoint)?.with_cancellation(token.clone());
    let marker = fixture.root.join("tmp").join(SLEEP_MARKER);
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| authority.run(&spec));
        wait_until_owned(authority)?;
        // Ownership alone is not enough: it is true before the leader has
        // exec'd, so a cancel issued here can beat the spawn and produce a
        // Cancelled report with no process group at all, making every
        // assertion about the termination vacuous (B5-DESIGN §9.M).
        wait_for_marker(&marker)?;
        // The first cancel takes effect; the second is a no-op. That is the
        // idempotence half `ORC-CLI-OPS-001` states, at the token level.
        assert!(token.cancel(), "the first cancel was already consumed");
        assert!(!token.cancel(), "cancellation is not idempotent");
        join_report(handle.join())
    })
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

#[test]
fn cancel_mid_phase_terminates_the_worker_and_leaves_no_process() -> TestResult {
    let fixture = Fixture::new("mid-phase")?;
    let report = run_and_cancel(&fixture, "cancel_fixture_sleep_entrypoint")?;
    assert_eq!(report.process.termination, ProcessTermination::Cancelled);
    assert!(report.process.cancellation_observed);
    assert!(report.process.term_sent, "no SIGTERM was sent: {report:#?}");
    assert!(report.process.direct_child_reaped, "{report:#?}");
    assert!(report.process.group_absent, "{report:#?}");
    assert!(report.process.cleanup_complete, "{report:#?}");
    assert!(!fixture.authority()?.has_unresolved_processes());
    assert_group_absent(report.process.pgid)
}

#[test]
fn cancel_removes_a_descendant_that_ignores_sigterm() -> TestResult {
    // "No descendants", measured. The shell member traps `SIGTERM`, so a
    // cleanup that stopped after the `SIGTERM` leg would leave the group
    // present and this case would fail rather than pass vacuously.
    let fixture = Fixture::new("descendant")?;
    let authority = fixture.authority()?;
    let token = CancellationToken::new();
    let spec =
        helper_spec("cancel_fixture_descendant_entrypoint")?.with_cancellation(token.clone());
    let marker = fixture.root.join("tmp").join(DESCENDANT_MARKER);

    let (report, descendant) = std::thread::scope(|scope| {
        let handle = scope.spawn(|| authority.run(&spec));
        wait_until_owned(authority)?;
        wait_for_marker(&marker)?;
        let descendant: u32 = fs::read_to_string(&marker)?.trim().parse()?;
        // The descendant must still be alive when the cancel is issued, or
        // "it did not survive" would hold for the wrong reason.
        assert_alive(descendant)?;
        assert!(token.cancel(), "the first cancel was already consumed");
        assert!(!token.cancel(), "cancellation is not idempotent");
        Ok::<_, Box<dyn std::error::Error>>((join_report(handle.join())?, descendant))
    })?;

    assert_eq!(report.process.termination, ProcessTermination::Cancelled);
    assert!(report.process.term_sent, "no SIGTERM was sent: {report:#?}");
    assert!(
        report.process.kill_sent,
        "the SIGTERM-ignoring descendant did not force a SIGKILL escalation: {report:#?}"
    );
    assert!(report.process.cleanup_complete, "{report:#?}");
    assert!(!authority.has_unresolved_processes());
    assert_pid_absent(descendant)?;
    assert_group_absent(report.process.pgid)
}

fn assert_alive(raw: u32) -> TestResult {
    let pid = Pid::from_raw(i32::try_from(raw)?).ok_or("invalid pid")?;
    test_kill_process(pid)
        .map_err(|error| format!("the announced descendant {raw} was already gone: {error}"))?;
    Ok(())
}

#[test]
fn cancel_after_a_terminal_outcome_changes_nothing() -> TestResult {
    let fixture = Fixture::new("terminal")?;
    let authority = fixture.authority()?;
    let token = CancellationToken::new();
    let spec = helper_spec("cancel_fixture_terminal_entrypoint")?.with_cancellation(token.clone());
    let report = authority.run(&spec)?;
    assert_eq!(report.process.termination, ProcessTermination::Exited(0));
    assert!(
        String::from_utf8_lossy(&report.process.stdout).contains("TERMINAL"),
        "the terminal entrypoint did not run: {report:#?}"
    );
    assert!(!authority.has_unresolved_processes());

    // Cancelling a mission that already reached terminal is accepted and does
    // nothing: the token flips exactly once and the supervisor still owns no
    // process.
    assert!(token.cancel());
    assert!(!token.cancel());
    assert!(!authority.has_unresolved_processes());
    assert_group_absent(report.process.pgid)
}

#[test]
fn cancellation_does_not_delete_a_pre_existing_path() -> TestResult {
    let fixture = Fixture::new("pre-existing")?;
    let note = fixture.parent.join("operator-note.txt");
    fs::write(&note, b"written before the run\n")?;
    let sibling = fixture.root.join("operator-sibling.txt");
    fs::write(&sibling, b"written before the run\n")?;

    let report = run_and_cancel(&fixture, "cancel_fixture_sleep_entrypoint")?;
    assert_eq!(report.process.termination, ProcessTermination::Cancelled);
    assert_eq!(fs::read(&note)?, b"written before the run\n");
    assert_eq!(fs::read(&sibling)?, b"written before the run\n");
    assert_group_absent(report.process.pgid)
}

// ---------------------------------------------------------------------------
// Cleanup confinement
// ---------------------------------------------------------------------------

#[test]
fn cleanup_cannot_escape_the_fixture_root() -> TestResult {
    // B5-DESIGN §4 Gate 4's negative assertion, at the only door that names a
    // cleanup destination: `TrashRoot::under` refuses anything that is not a
    // single ordinary component, *before* any unlink can be attempted.
    let fixture = Fixture::new("confinement")?;
    let capability = &fixture.git_capability;
    for component in [
        "..",
        "../escape",
        "/absolute",
        "nested/component",
        ".",
        "",
        "trash/../..",
    ] {
        let refused = TrashRoot::under(capability, component);
        assert!(
            refused.is_err(),
            "TrashRoot accepted the escaping component {component:?}"
        );
    }
    // A single ordinary component is accepted, so the refusals above are a
    // rule rather than a constructor that never succeeds.
    let accepted = TrashRoot::under(capability, "trash")?;
    drop(accepted);

    // And a symlink named as an ordinary component still cannot redirect the
    // destination: the capability root is fixed, so the resolved path stays
    // beneath it whatever the name points at.
    let outside = fixture.parent.join("outside-the-root");
    fs::create_dir_all(&outside)?;
    std::os::unix::fs::symlink(&outside, fixture.root.join("symlinked-trash"))?;
    let symlinked = TrashRoot::under(capability, "symlinked-trash")?;
    drop(symlinked);
    assert!(
        fs::read_dir(&outside)?.next().is_none(),
        "naming a symlinked component wrote outside the fixture root"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The oracle's terminal-cancel semantics
// ---------------------------------------------------------------------------

/// A synthetic Go-readable home holding one completed workspace.
struct OracleHome {
    parent: PathBuf,
}

impl Drop for OracleHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

impl OracleHome {
    fn new(label: &str) -> TestResult<Self> {
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let parent = fs::canonicalize(std::env::temp_dir())?.join(format!(
            "orchestrator-rs-b5-cancel-oracle-{}-{number}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&parent);
        private_dir(&parent)?;
        let home = Self { parent };
        for leaf in ["user", "personas", "empty-path"] {
            fs::create_dir_all(home.parent.join(leaf))?;
        }
        let workspace = home.workspace();
        fs::create_dir_all(&workspace)?;
        fs::write(workspace.join("mission.md"), b"a fixture mission\n")?;
        fs::write(workspace.join("checkpoint.json"), COMPLETED_CHECKPOINT)?;
        fs::write(workspace.join("operator-note.txt"), b"written before\n")?;
        Ok(home)
    }

    fn home(&self) -> PathBuf {
        self.parent.join("home")
    }

    fn workspace(&self) -> PathBuf {
        self.home().join("workspaces").join("ws-terminal")
    }

    fn observe(&self, binary: &Path, argv: &[&str]) -> io::Result<(Option<i32>, String, String)> {
        let output = Command::new(binary)
            .args(argv)
            .current_dir(&self.parent)
            .env_clear()
            .env("HOME", self.parent.join("user"))
            .env("ORCHESTRATOR_CONFIG_DIR", self.home())
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
}

const COMPLETED_CHECKPOINT: &[u8] = br#"{"version":1,"payload":{"version":2,"workspace_id":"ws-terminal","domain":"dev","plan":{"id":"plan-terminal","task":"fixture task","phases":[{"id":"phase-1","name":"build","status":"completed"}]},"status":"completed","started_at":"2026-07-13T00:00:00Z"}}"#;

#[test]
fn the_oracle_cancels_a_terminal_mission_idempotently() -> TestResult {
    // `compatibility/contracts.yaml` ORC-CLI-OPS-001: "cancel validates
    // workspace containment, is idempotent for terminal missions". Asserted
    // against the binary that makes the claim, over a workspace that is
    // already terminal, with the durable state compared before and after both
    // invocations.
    let go_binary = frozen_go_oracle()?;
    let home = OracleHome::new("idempotent")?;
    let before = fs::read(home.workspace().join("checkpoint.json"))?;

    let first = home.observe(&go_binary, &["cancel", "ws-terminal"])?;
    let second = home.observe(&go_binary, &["cancel", "ws-terminal"])?;
    assert_eq!(first.0, Some(0), "first cancel: {first:?}");
    assert_eq!(second.0, Some(0), "second cancel: {second:?}");
    assert_eq!(
        first.1, second.1,
        "the second cancel of a terminal mission rendered differently"
    );
    assert!(
        first.1.contains("already completed"),
        "the oracle did not recognise the mission as terminal: {first:?}"
    );
    assert_eq!(
        fs::read(home.workspace().join("checkpoint.json"))?,
        before,
        "cancelling a terminal mission rewrote its checkpoint"
    );
    assert_eq!(
        fs::read(home.workspace().join("operator-note.txt"))?,
        b"written before\n",
        "cancelling a terminal mission deleted a path it did not create"
    );
    Ok(())
}

#[test]
fn the_oracle_refuses_a_traversing_workspace_id() -> TestResult {
    let go_binary = frozen_go_oracle()?;
    let home = OracleHome::new("traversal")?;
    let outside = home.parent.join("outside-the-home");
    fs::create_dir_all(&outside)?;
    fs::write(outside.join("survivor.txt"), b"untouched\n")?;

    for argument in ["../outside-the-home", "../..", "/etc", "no-such-workspace"] {
        let (exit, stdout, _) = home.observe(&go_binary, &["cancel", argument])?;
        assert_ne!(exit, Some(0), "the oracle accepted cancel {argument:?}");
        assert!(
            stdout.is_empty(),
            "a refused cancel wrote to stdout for {argument:?}"
        );
    }
    assert_eq!(fs::read(outside.join("survivor.txt"))?, b"untouched\n");
    Ok(())
}

#[test]
fn the_rust_cancel_surface_is_a_recorded_gap_not_a_skip() -> TestResult {
    // `cancel` and `cleanup` are in `is_go_command`, so the Rust CLI refuses
    // them. Both halves are asserted so the row cannot decay into "the test did
    // not run": the oracle must implement the surface and the Rust side must
    // refuse it in the exact shape pinned here.
    let go_binary = frozen_go_oracle()?;
    let rust_binary = PathBuf::from(env!("CARGO_BIN_EXE_orchestrator"));
    let home = OracleHome::new("gap")?;
    for argv in [
        vec!["cancel", "--help"],
        vec!["cleanup", "--help"],
        vec!["cancel", "ws-terminal"],
    ] {
        let (go_exit, go_stdout, _) = home.observe(&go_binary, &argv)?;
        assert_eq!(
            go_exit,
            Some(0),
            "{argv:?}: the oracle refused its own surface"
        );
        assert!(
            !go_stdout.is_empty(),
            "{argv:?}: the oracle produced nothing, so this is not a Rust gap"
        );

        let (rust_exit, rust_stdout, rust_stderr) = home.observe(&rust_binary, &argv)?;
        assert_eq!(
            rust_exit,
            Some(1),
            "{argv:?}: the refusal's exit status moved"
        );
        assert!(
            rust_stdout.is_empty(),
            "{argv:?}: a refusing command wrote to stdout"
        );
        assert_eq!(
            rust_stderr.trim(),
            "unsupported architecture-foundation command",
            "{argv:?}: the refusal's wording moved"
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
