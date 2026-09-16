//! B5-DESIGN §4, Gate 1b — the crash cut of the composition root.
//!
//! Kept in its own binary because a crash gate must not share a process with
//! the cases it can take down.
//!
//! **Every case uses a real crash.** The test binary re-execs itself in a
//! `crash-at-<seal>` mode, the child parks at a durable barrier behind a marker
//! file, and the parent kills its whole process group with `SIGKILL`, asserting
//! `status.signal() == Some(9)` so a clean exit cannot pass for a crash. This is
//! CF-M3-W8's mechanism. An in-process `drop`-simulated crash would assert
//! states no real death can reach — the finding that flipped `git_run_e2e`'s C3
//! — so none is used here.
//!
//! The child never calls a door the parent could not: it recovers the same
//! fixture authority with the same policy-pinned helper bytes, seals through
//! the same `composition::seal`, and dies. What differs per case is only *how
//! far* it got before the kill.
//!
//! Load average is recorded for every run of this gate, per §4's
//! timing-barrier protocol: [`load_average_is_recorded_for_this_gate`] writes
//! it to a fixed file and fails if it could not.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use std::os::unix::process::{CommandExt, ExitStatusExt};

use orchestrator_app::{
    FixtureAdmissionPolicy, FreshFixtureAuthority, IsolatedFixtureRoot, PhaseMetricIntent,
};
use orchestrator_cli::{
    FixtureEnrollment, PersistentFlags, ResolvedRun, RunFlags, SealedRun, resolve_with_context,
    seal,
};
use orchestrator_core::{MissionId, PhaseId};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const FIXTURE_RUNTIME: &str = "codex";
const HELPER: &str = "attested-helper";
const CRASH_AT: &str = "NANIKA_B5_CRASH_AT";
const CRASH_ROOT: &str = "NANIKA_B5_CRASH_ROOT";
const CRASH_PARENT: &str = "NANIKA_B5_CRASH_PARENT";
const CRASH_MISSION: &str = "NANIKA_B5_CRASH_MISSION";
const CRASH_HELPER: &str = "NANIKA_B5_CRASH_HELPER";
const CRASH_HELPER_ARGUMENTS: &str = "NANIKA_B5_CRASH_HELPER_ARGUMENTS";
/// Argument vectors for the attested helper. Both run *this* binary — a thin,
/// in-tree build artifact — so the gate needs no helper of its own and no
/// system executable. `is_native_executable` rejects a macOS universal binary
/// (`/bin/sleep` and every other system tool is fat), and B5-DESIGN §5.1
/// declines K0.1B's helper mains as superseded, so this is the only attested
/// long-runner the CLI crate can reach.
const HELPER_QUICK: &[&str] = &["--exact", "helper_quick_entrypoint", "--nocapture"];
const HELPER_PARK: &[&str] = &["--exact", "helper_park_entrypoint", "--nocapture"];
/// How long the parked child waits to be killed before giving up. A child that
/// reaches this has not been killed, and the parent's signal assertion fails.
const PARK_LIMIT: Duration = Duration::from_secs(120);
const BARRIER_DEADLINE: Duration = Duration::from_secs(60);

static CASE: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------------------
// The durable home both processes share
// ---------------------------------------------------------------------------

/// A fixture home the parent creates and then *lets go of*, so a child process
/// can recover the same authority over it.
///
/// `FreshFixtureAuthority` holds an exclusive `flock` on the root, so the
/// parent's admission is dropped before any child runs; the child recovers,
/// dies under `SIGKILL` (which the kernel unlocks), and the parent recovers
/// again. That sequence is the whole point: the lease protocol has to survive a
/// process that never got to release anything.
struct Home {
    parent: PathBuf,
    root: PathBuf,
    mission: MissionId,
    helper: PathBuf,
    helper_arguments: Vec<String>,
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

impl Home {
    fn new(label: &str, helper_arguments: &[&str]) -> TestResult<Self> {
        let helper = std::env::current_exe()?;
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let temporary = std::fs::canonicalize(std::env::temp_dir())?;
        let parent = temporary.join(format!(
            "orchestrator-rs-b5-crash-{}-{number}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&parent);
        private_dir(&parent)?;

        let bytes = std::fs::read(&helper)?;
        let root = {
            let fresh = IsolatedFixtureRoot::create_fresh(&parent)?;
            let path = fresh.path().to_path_buf();
            // Admit once so the root carries the v2 marker `recover` requires,
            // then drop the authority so the exclusive lock is free.
            let authority = FreshFixtureAuthority::admit(fresh, &policy(&parent, &bytes)?)?;
            drop(authority);
            path
        };
        Ok(Self {
            parent,
            root,
            mission: MissionId::new(format!("b5-crash-{number}"))?,
            helper,
            helper_arguments: helper_arguments
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
        })
    }

    fn marker(&self, at: &str) -> PathBuf {
        self.parent.join(format!("parked-{at}"))
    }

    fn events(&self) -> Vec<(String, String)> {
        read_events(&self.root, self.mission.as_str())
    }

    fn metrics(&self, table: &str, column: &str) -> TestResult<i64> {
        metrics_rows(&self.root, self.mission.as_str(), table, column)
    }

    /// Recovers the authority and drives the mission to terminal in *this*
    /// process, exactly as the child would have.
    fn resume(&self) -> TestResult<bool> {
        let bytes = std::fs::read(&self.helper)?;
        let policy = policy(&self.parent, &bytes)?;
        let authority =
            FreshFixtureAuthority::recover(IsolatedFixtureRoot::identify(&self.root)?, &policy)?;
        let root = IsolatedFixtureRoot::identify(&self.root)?;
        let arguments: Vec<&str> = self.helper_arguments.iter().map(String::as_str).collect();
        let enrollment = FixtureEnrollment {
            root: &root,
            authority: &authority,
            helper_label: HELPER,
            helper_bytes: &bytes,
            helper_arguments: &arguments,
            mission: self.mission.clone(),
            phase: PhaseId::new("phase-1")?,
            runtime: FIXTURE_RUNTIME,
            git: None,
            knowledge: None,
            evidence: None,
        };
        let mut sealed = seal(&ordinary_flags(), Some(&enrollment))?;
        let resolved = mission_plan(&sealed)?;
        let report = sealed.execute(&resolved)?;
        Ok(report
            .phases
            .iter()
            .all(|run| run.result.outcome().is_completed()))
    }
}

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
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
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

fn mission_plan(sealed: &SealedRun<'_>) -> TestResult<ResolvedRun> {
    Ok(resolve_with_context(
        &[mission_source()],
        sealed.resolution(),
    )?)
}

fn read_events(root: &Path, mission: &str) -> Vec<(String, String)> {
    let path = root.join("events").join(format!("{mission}.jsonl"));
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .map(|value| {
            (
                value
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                value
                    .get("phase_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect()
}

fn metrics_rows(root: &Path, mission: &str, table: &str, column: &str) -> TestResult<i64> {
    let database = root.join("metrics.db");
    if !database.exists() {
        return Ok(0);
    }
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let sql = format!("SELECT count(*) FROM {table} WHERE {column} = ?1");
    Ok(connection.query_row(&sql, [mission], |row| row.get(0))?)
}

// ---------------------------------------------------------------------------
// The crash mechanism
// ---------------------------------------------------------------------------

/// What the parent waits for before it kills.
#[derive(Clone, Copy)]
enum Barrier {
    /// A marker file the child writes once it has reached the named state.
    Marker,
    /// The durable `worker.spawned` event, with no terminal after it. Used by
    /// C3, where a marker file would let the kill race the event append.
    SpawnedEvent,
}

/// Spawns this binary in its own process group in `at` mode, waits for the
/// durable barrier, then `SIGKILL`s the whole group.
///
/// Killing the *group* rather than the child matters: a case that parks with a
/// live helper (C3) would otherwise orphan it.
fn crash_at(home: &Home, at: &str, barrier: Barrier) -> TestResult {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "crash_child_entrypoint", "--nocapture"])
        .env(CRASH_AT, at)
        .env(CRASH_ROOT, &home.root)
        .env(CRASH_PARENT, &home.parent)
        .env(CRASH_MISSION, home.mission.as_str())
        .env(CRASH_HELPER, &home.helper)
        .env(CRASH_HELPER_ARGUMENTS, home.helper_arguments.join("\u{1f}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command.process_group(0);
    let mut child = command.spawn()?;

    let reached = await_barrier(home, at, barrier, &mut child)?;
    if !reached {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("the {at} child never reached its durable barrier").into());
    }

    kill_group(child.id());
    let status = child.wait()?;
    let signal = status.signal();
    if signal != Some(9) {
        return Err(format!(
            "the {at} child must die by SIGKILL, observed signal {signal:?} code {:?}",
            status.code()
        )
        .into());
    }
    Ok(())
}

fn await_barrier(home: &Home, at: &str, barrier: Barrier, child: &mut Child) -> TestResult<bool> {
    let marker = home.marker(at);
    let deadline = Instant::now() + BARRIER_DEADLINE;
    while Instant::now() < deadline {
        let reached = match barrier {
            Barrier::Marker => marker.exists(),
            Barrier::SpawnedEvent => {
                let events = home.events();
                events.len() == 1 && events[0].0 == "worker.spawned"
            }
        };
        if reached {
            return Ok(true);
        }
        if let Some(status) = child.try_wait()? {
            let mut reason = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                use std::io::Read;
                let _ = stderr.read_to_string(&mut reason);
            }
            return Err(format!(
                "the crash child exited early with {status:?}: {}",
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

/// The re-exec entry point. Inert unless [`CRASH_AT`] is set, so an ordinary
/// run of this binary never enters it.
#[test]
fn crash_child_entrypoint() {
    let Ok(at) = std::env::var(CRASH_AT) else {
        return;
    };
    if let Err(error) = run_crash_child(&at) {
        eprintln!("crash child {at} failed before its barrier: {error}");
        std::process::exit(2);
    }
}

fn run_crash_child(at: &str) -> TestResult {
    let root_path = PathBuf::from(std::env::var(CRASH_ROOT)?);
    let parent = PathBuf::from(std::env::var(CRASH_PARENT)?);
    let mission = MissionId::new(std::env::var(CRASH_MISSION)?)?;
    let helper_path = PathBuf::from(std::env::var(CRASH_HELPER)?);
    let helper_arguments: Vec<String> = std::env::var(CRASH_HELPER_ARGUMENTS)?
        .split('\u{1f}')
        .map(str::to_owned)
        .collect();
    let bytes = std::fs::read(&helper_path)?;
    let policy = policy(&parent, &bytes)?;
    let marker = parent.join(format!("parked-{at}"));

    match at {
        // C1 — the durable state seal 3 leaves: the store is open, the
        // canonical event owner is not yet installed.
        "seal-3" => {
            let root = IsolatedFixtureRoot::identify(&root_path)?;
            let _boundary = orchestrator_app::fixture_production_boundary(&root)?;
            let _store = orchestrator_app::open_fixture_runtime_store(&root)?;
            park(&marker)
        }
        // C2 — the durable state seal 5 leaves: the attested helper is
        // installed, the provider is not yet admitted.
        "seal-5" => {
            let root = IsolatedFixtureRoot::identify(&root_path)?;
            let authority = FreshFixtureAuthority::recover(root, &policy)?;
            let _executable = authority.install_fixture_executable(HELPER, &bytes)?;
            park(&marker)
        }
        // C3 — mid-phase: the seal is complete, `worker.spawned` is durable,
        // and the helper is still running when the group is killed.
        // C4 — the metrics-commit-before-acknowledgement window: the mission
        // row is committed and the parent kills before anything acknowledges.
        // C6 — seal 13: the run finished but the sealed root is still held, so
        // no lease was released and no store was closed.
        "phase" | "metrics" | "shutdown" => {
            let root = IsolatedFixtureRoot::identify(&root_path)?;
            // Two handles over the same canonical path: `recover` consumes
            // one to take the authority, and the seal's `root` is the other.
            let authority = FreshFixtureAuthority::recover(
                IsolatedFixtureRoot::identify(&root_path)?,
                &policy,
            )?;
            let arguments: Vec<&str> = helper_arguments.iter().map(String::as_str).collect();
            let enrollment = FixtureEnrollment {
                root: &root,
                authority: &authority,
                helper_label: HELPER,
                helper_bytes: &bytes,
                helper_arguments: &arguments,
                mission: mission.clone(),
                phase: PhaseId::new("phase-1")?,
                runtime: FIXTURE_RUNTIME,
                git: None,
                knowledge: None,
                evidence: None,
            };
            let mut sealed = seal(&ordinary_flags(), Some(&enrollment))?;
            let resolved = mission_plan(&sealed)?;
            if at == "phase" {
                // No marker: the parent's barrier is the durable
                // `worker.spawned` event, so the kill cannot race the append.
                // The park helper keeps the phase open until the kill lands.
                let _report = sealed.execute(&resolved)?;
                return sleep_until_killed();
            }
            let _report = sealed.execute(&resolved)?;
            if at == "metrics" {
                sealed.record_phase(&PhaseMetricIntent::new(
                    mission,
                    "phase-1",
                    1,
                    reaped_witness()?,
                ))?;
            }
            // `sealed` is a scope-local with a `Drop`, so seal 13 runs only if
            // this process is *not* killed. A `SIGKILL` while it is parked
            // means neither the metrics lease nor the store was ever released,
            // which is exactly the state C6 restarts from.
            std::fs::write(&marker, b"parked\n")?;
            sleep_until_killed()
        }
        other => Err(format!("unknown crash point {other}").into()),
    }
}

fn park(marker: &Path) -> TestResult {
    std::fs::write(marker, b"parked\n")?;
    sleep_until_killed()
}

fn sleep_until_killed() -> TestResult {
    let deadline = Instant::now() + PARK_LIMIT;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("the parked child was never killed".into())
}

// ---------------------------------------------------------------------------
// Helpers the cases share
// ---------------------------------------------------------------------------

/// The terminal state a run of the same mission reaches when nothing crashes.
struct Reference {
    events: Vec<(String, String)>,
    missions: i64,
}

fn crash_free_reference(label: &str) -> TestResult<Reference> {
    let home = Home::new(label, HELPER_QUICK)?;
    assert!(home.resume()?, "the reference run must reach terminal");
    Ok(Reference {
        events: home.events(),
        missions: home.metrics("missions", "id")?,
    })
}

fn assert_converged(home: &Home, reference: &Reference) -> TestResult {
    assert_eq!(
        home.events(),
        reference.events,
        "a restart after a crash before any event must converge on the crash-free sequence"
    );
    assert_eq!(home.metrics("missions", "id")?, reference.missions);
    Ok(())
}

// ---------------------------------------------------------------------------
// C1 — crash between seal 3 and seal 4
// ---------------------------------------------------------------------------

#[test]
fn c1_crash_between_the_store_and_the_event_owner_converges_on_restart() -> TestResult {
    let reference = crash_free_reference("c1-reference")?;
    let home = Home::new("c1", HELPER_QUICK)?;
    crash_at(&home, "seal-3", Barrier::Marker)?;

    // No orphan lease: the next seal succeeds in this process.
    assert!(home.resume()?, "the restart must reach terminal");
    assert_converged(&home, &reference)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// C2 — crash between seal 5 and seal 6
// ---------------------------------------------------------------------------

#[test]
fn c2_crash_after_installing_the_helper_re_admits_rather_than_reinstalls() -> TestResult {
    let reference = crash_free_reference("c2-reference")?;
    let home = Home::new("c2", HELPER_QUICK)?;
    crash_at(&home, "seal-5", Barrier::Marker)?;

    let installed = home.root.join("bin");
    let before: Vec<String> = std::fs::read_dir(&installed)?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        before,
        vec![HELPER.to_owned()],
        "the crashed child left exactly one installed helper"
    );
    let identity_before = std::fs::metadata(installed.join(HELPER))?;

    assert!(home.resume()?, "the restart must reach terminal");

    let after: Vec<String> = std::fs::read_dir(&installed)?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(after, before, "no duplicate executable");
    let identity_after = std::fs::metadata(installed.join(HELPER))?;
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        (identity_before.dev(), identity_before.ino()),
        (identity_after.dev(), identity_after.ino()),
        "the installed helper must be re-admitted, not reinstalled"
    );
    assert_converged(&home, &reference)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// C3 — crash mid-phase, after `worker.spawned` and before the terminal event
// ---------------------------------------------------------------------------

#[test]
fn c3_crash_mid_phase_resumes_without_duplicating_the_phase_row() -> TestResult {
    // The park helper keeps the phase open, so the barrier is the *durable*
    // `worker.spawned` event rather than a marker file the child could write
    // before the event was committed.
    let home = Home::new("c3", HELPER_PARK)?;
    crash_at(&home, "phase", Barrier::SpawnedEvent)?;

    let during = home.events();
    assert_eq!(
        during,
        vec![("worker.spawned".to_owned(), "phase-1".to_owned())],
        "the kill must land after the spawn and before the terminal event"
    );

    // The restart runs the same attested bytes with the quick argument vector:
    // the durable state, not the helper's duration, is what has to converge.
    let resumed = Home {
        parent: home.parent.clone(),
        root: home.root.clone(),
        mission: home.mission.clone(),
        helper: home.helper.clone(),
        helper_arguments: HELPER_QUICK
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
    };
    let reached = resumed.resume();
    // `resumed` shares the parent directory with `home`, which owns the
    // cleanup, so its own destructor must not remove the tree.
    std::mem::forget(resumed);
    assert!(reached?, "the resumed phase must reach terminal");

    let after = home.events();
    assert_eq!(
        after.last().map(|(kind, _)| kind.as_str()),
        Some("worker.completed"),
        "the resumed attempt must end on a terminal event, observed {after:?}"
    );
    assert_eq!(
        after
            .iter()
            .filter(|(kind, _)| kind == "worker.completed")
            .count(),
        1,
        "exactly one terminal event, observed {after:?}"
    );
    assert_eq!(
        after
            .iter()
            .filter(|(kind, _)| kind == "worker.spawned")
            .count(),
        2,
        "the pre-crash spawn is retained and the resumed attempt adds one, \
         which is what makes the single terminal above meaningful: {after:?}"
    );
    assert_eq!(
        home.metrics("missions", "id")?,
        1,
        "no duplicate mission row"
    );
    assert_eq!(
        home.metrics("phases", "mission_id")?,
        0,
        "no phase row is written for a phase whose reaping was never witnessed"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// C4 — crash in the metrics-commit-before-acknowledgement window
// ---------------------------------------------------------------------------

#[test]
fn c4_crash_after_the_metrics_commit_leaves_the_crash_free_rows() -> TestResult {
    let reference = crash_free_reference("c4-reference")?;
    let home = Home::new("c4", HELPER_QUICK)?;
    crash_at(&home, "metrics", Barrier::Marker)?;

    // The committed rows survived the kill even though nothing acknowledged.
    assert_eq!(home.metrics("missions", "id")?, reference.missions);
    assert_eq!(home.metrics("phases", "mission_id")?, 1);

    assert!(home.resume()?, "the restart must reach terminal");
    assert_eq!(
        home.metrics("missions", "id")?,
        reference.missions,
        "the restart must not add a second mission row"
    );
    assert_eq!(
        home.metrics("phases", "mission_id")?,
        1,
        "the phase row is keyed on mission+phase, so a restart upserts it"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// C5 — anti-vacuity control
// ---------------------------------------------------------------------------

#[test]
fn c5_the_same_run_without_a_crash_reaches_terminal() -> TestResult {
    let home = Home::new("c5", HELPER_QUICK)?;
    assert!(
        home.resume()?,
        "the uncrashed run must reach terminal, or C1-C4's convergence is vacuous"
    );
    assert_eq!(
        home.events(),
        vec![
            ("worker.spawned".to_owned(), "phase-1".to_owned()),
            ("worker.completed".to_owned(), "phase-1".to_owned()),
        ]
    );
    assert_eq!(home.metrics("missions", "id")?, 1);
    Ok(())
}

// ---------------------------------------------------------------------------
// C6 — crash during seal 13 shutdown
// ---------------------------------------------------------------------------

#[test]
fn c6_crash_during_shutdown_releases_every_lease_and_records_no_phantom_phase() -> TestResult {
    let home = Home::new("c6", HELPER_QUICK)?;
    crash_at(&home, "shutdown", Barrier::Marker)?;

    // The child died holding the sealed root, so seal 13 never ran: no store
    // close, no lease release, no cancellation. The kernel released the locks;
    // a fresh seal in this process proves it.
    assert!(
        home.resume()?,
        "every lease the killed process held must be reclaimable"
    );
    assert_eq!(
        home.metrics("phases", "mission_id")?,
        0,
        "no metrics row for a phase that never released"
    );
    assert_eq!(
        home.metrics("missions", "id")?,
        1,
        "the mission row the killed process committed is not duplicated"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The recorded load average
// ---------------------------------------------------------------------------

/// B5-DESIGN §4: "A green run of Gate 1b with no recorded load average is not
/// evidence." This writes it where the ledger can cite it and fails if it
/// could not be read.
#[test]
fn load_average_is_recorded_for_this_gate() -> TestResult {
    let output = Command::new("/usr/bin/uptime").output()?;
    let recorded = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    assert!(
        !recorded.is_empty(),
        "the load average must be observable for this gate"
    );
    let path = std::env::temp_dir().join("orchestrator-rs-b5-crash-load.txt");
    std::fs::write(&path, format!("{recorded}\n"))?;
    println!("gate 1b load average: {recorded}");
    println!("recorded to {}", path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// The reaping witness
// ---------------------------------------------------------------------------

fn reaped_witness() -> TestResult<orchestrator_app::ExactProcessGroupAbsence> {
    use orchestrator_app::{
        KernelProcessIdentity, RecordedProcessIdentityStatus, inspect_recorded_process_identity,
    };
    use std::io::BufRead;

    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "reaping_witness_entrypoint", "--nocapture"])
        .env("NANIKA_B5_CRASH_WITNESS", "1")
        .env_remove(CRASH_AT)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    command.process_group(0);
    let mut child = command.spawn()?;
    let pid = child.id();
    {
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

/// The attested helper's quick argument vector lands here and returns at once.
#[test]
fn helper_quick_entrypoint() {}

/// The attested helper's parking argument vector lands here.
///
/// It parks only when this binary was invoked with its own name as an exact
/// filter, which is what the helper argument vector does and what an ordinary
/// run of this gate never does — a plain `cargo test` run passes no filter, so
/// the guard is false and the test returns immediately.
#[test]
fn helper_park_entrypoint() {
    if !std::env::args().any(|argument| argument == "helper_park_entrypoint") {
        return;
    }
    std::thread::sleep(PARK_LIMIT);
}

/// Re-exec entry point for [`reaped_witness`]. Inert unless re-executed.
#[test]
fn reaping_witness_entrypoint() {
    if std::env::var("NANIKA_B5_CRASH_WITNESS").is_err() {
        return;
    }
    use std::io::{BufRead, Write};
    println!("ready");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    std::process::exit(0);
}
