#![cfg(unix)]

use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use orchestrator_app::KernelProcessIdentity;
use orchestrator_app::{
    IsolatedHomeGuards, RUST_PILOT_PROCESS_OWNERSHIP_FILE, RuntimeStoreError,
    RustPilotProcessAttempt, RustPilotProcessError, RustPilotProcessExecutable,
    RustPilotProcessOpen, RustPilotProcessRecovery, RustPilotProcessSession, RustPilotRuntimeHome,
};
use orchestrator_core::MissionId;
use orchestrator_exec::{ProcessBudget, ProcessPurpose, ProcessRequest, ProcessTerminationReceipt};
use rusqlite::{Connection, params};

const ACTION: &str = "NANIKA_PILOT_PROCESS_TEST_ACTION";
const TARGET: &str = "NANIKA_PILOT_PROCESS_TEST_TARGET";
const ACTUAL_HOME: &str = "NANIKA_PILOT_PROCESS_TEST_ACTUAL_HOME";
const GUARD_HOME: &str = "NANIKA_PILOT_PROCESS_TEST_GUARD_HOME";
const CHECKOUT: &str = "NANIKA_PILOT_PROCESS_TEST_CHECKOUT";
const EXECUTABLE: &str = "NANIKA_PILOT_PROCESS_TEST_EXECUTABLE";
const BARRIER: &str = "NANIKA_PILOT_PROCESS_TEST_BARRIER";
const READY: &str = "NANIKA_PILOT_PROCESS_TEST_READY";
const RELEASE: &str = "NANIKA_PILOT_PROCESS_TEST_RELEASE";
const BLOCK: &str = "NANIKA_PILOT_PROCESS_TEST_BLOCK";
const PILOT: &str = "NANIKA_RUST_FIRST_USE_PILOT";
const VERSION: &str = "process-composition-test-v1";

static NEXT_CASE: AtomicU64 = AtomicU64::new(1);

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Case {
    parent: PathBuf,
    actual_home: PathBuf,
    guard_home: PathBuf,
    checkout: PathBuf,
    target: PathBuf,
    executable: PathBuf,
}

impl Case {
    fn new(label: &str) -> TestResult<Self> {
        let suffix = NEXT_CASE.fetch_add(1, Ordering::Relaxed);
        let parent = fs::canonicalize(std::env::temp_dir())?.join(format!(
            "orchestrator-rust-pilot-process-{label}-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir(&parent)?;
        private_directory(&parent)?;
        let actual_home = parent.join("actual-home");
        let guard_home = parent.join("guard-home");
        let checkout = parent.join("checkout");
        for directory in [&actual_home, &guard_home, &checkout] {
            fs::create_dir(directory)?;
            private_directory(directory)?;
        }
        let executable = parent.join("native-target.sh");
        private_file(
            &executable,
            b"#!/bin/sh\nIFS= read -r input\nprintf 'stdout:%s:%s:%s:%s\\n' \"$1\" \"$NANIKA_EXACT_ENV\" \"$input\" \"$PWD\"\nprintf 'stderr:%s\\n' \"$2\" >&2\nprintf x >> \"$3\"\nif [ -n \"$NANIKA_BLOCK_FILE\" ]; then\n  while [ ! -f \"$NANIKA_BLOCK_FILE\" ]; do sleep 1; done\nfi\n",
            0o700,
        )?;
        let executable = fs::canonicalize(executable)?;
        Ok(Self {
            target: parent.join("pilot-home"),
            parent,
            actual_home,
            guard_home,
            checkout,
            executable,
        })
    }

    fn command(&self, action: &str) -> TestResult<Command> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--exact", "child_helper", "--nocapture"])
            .env_remove(ACTION)
            .env_remove(TARGET)
            .env_remove(ACTUAL_HOME)
            .env_remove(GUARD_HOME)
            .env_remove(CHECKOUT)
            .env_remove(EXECUTABLE)
            .env_remove(BARRIER)
            .env_remove(READY)
            .env_remove(RELEASE)
            .env_remove(BLOCK)
            .env_remove(PILOT)
            .env("HOME", &self.actual_home)
            .env(ACTION, action)
            .env(TARGET, &self.target)
            .env(ACTUAL_HOME, &self.actual_home)
            .env(GUARD_HOME, &self.guard_home)
            .env(CHECKOUT, &self.checkout)
            .env(EXECUTABLE, &self.executable)
            .env(PILOT, "1");
        Ok(command)
    }

    fn run(&self, action: &str) -> TestResult<Output> {
        Ok(self.command(action)?.output()?)
    }

    fn crash_at(&self, barrier: &str, action: &str) -> TestResult {
        let ready = self.parent.join(format!("{barrier}.ready"));
        let block = self.parent.join(format!("{barrier}.block"));
        let mut command = self.command(action)?;
        command
            .env(BARRIER, barrier)
            .env(READY, &ready)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if barrier == "started" {
            command.env(BLOCK, &block);
        }
        let mut child = command.spawn()?;
        wait_for_path(&ready, &mut child)?;
        if barrier == "started" {
            wait_for_counter(&self.target.join("crash-after.counter"), &mut child, b"x")?;
        }
        child.kill()?;
        let status = child.wait()?;
        assert!(!status.success());
        Ok(())
    }

    fn fail_ownership_persistence(&self) -> TestResult<Output> {
        let ready = self.parent.join("ownership.ready");
        let release = self.parent.join("ownership.release");
        let mut command = self.command("ownership-error")?;
        command
            .env(BARRIER, "claimed")
            .env(READY, &ready)
            .env(RELEASE, &release);
        let mut child = command.spawn()?;
        wait_for_path(&ready, &mut child)?;
        let ownership = self.target.join(RUST_PILOT_PROCESS_OWNERSHIP_FILE);
        fs::remove_file(&ownership)?;
        std::os::unix::fs::symlink(self.target.join("ownership-error.counter"), &ownership)?;
        private_file(&release, b"release\n", 0o600)?;
        Ok(child.wait_with_output()?)
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

fn private_directory(path: &Path) -> std::io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn private_file(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn append_mission_journal_record(
    writer: &orchestrator_app::ProductionWriterAuthority,
    path: &Path,
    bytes: &[u8],
) -> TestResult {
    writer.boundary().verify()?;
    private_file(path, bytes, 0o600)?;
    writer.boundary().verify()?;
    Ok(())
}

fn wait_for_path(path: &Path, child: &mut std::process::Child) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if path.is_file() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(format!("child exited before barrier with {status}").into());
        }
        if Instant::now() >= deadline {
            return Err("child did not reach durable barrier".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_counter(path: &Path, child: &mut std::process::Child, expected: &[u8]) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if fs::read(path).ok().as_deref() == Some(expected) {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(format!("child exited before target counter with {status}").into());
        }
        if Instant::now() >= deadline {
            return Err("target did not write its exact counter".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn replace_recorded_process_group(database: &Path, replacement: u32) -> TestResult<u32> {
    let connection = Connection::open(database)?;
    let original = connection.query_row(
        "SELECT process_group_id FROM outbox_execution_identity LIMIT 1",
        [],
        |row| row.get::<_, u32>(0),
    )?;
    connection.execute_batch("DROP TRIGGER outbox_execution_identity_reject_update;")?;
    connection.execute(
        "UPDATE outbox_execution_identity SET process_group_id = ?1",
        params![replacement],
    )?;
    connection.execute_batch(
        "CREATE TRIGGER outbox_execution_identity_reject_update
                    BEFORE UPDATE ON outbox_execution_identity
                    BEGIN SELECT RAISE(ABORT, 'process execution identities are immutable'); END;",
    )?;
    connection.close().map_err(|(_, error)| error)?;
    Ok(original)
}

fn child_path(name: &str) -> TestResult<PathBuf> {
    Ok(PathBuf::from(std::env::var_os(name).ok_or(name)?))
}

fn acquire() -> TestResult<orchestrator_app::ProductionWriterAuthority> {
    let guards = IsolatedHomeGuards {
        user_home: child_path(GUARD_HOME)?,
        repository_checkout: child_path(CHECKOUT)?,
    };
    Ok(RustPilotRuntimeHome::acquire(
        &child_path(TARGET)?,
        &guards,
        VERSION,
    )?)
}

fn ensure_cwd() -> TestResult<PathBuf> {
    let cwd = child_path(TARGET)?.join("private-cwd");
    if !cwd.exists() {
        fs::create_dir(&cwd)?;
        private_directory(&cwd)?;
    }
    Ok(fs::canonicalize(cwd)?)
}

fn request(
    purpose: ProcessPurpose,
    executable_id: &str,
    cwd: &Path,
    counter: &Path,
) -> TestResult<ProcessRequest> {
    let mut request = ProcessRequest::new(purpose, executable_id, cwd)?
        .with_argument("exact-argv")?
        .with_argument("exact-error")?
        .with_argument(counter.to_string_lossy())?
        .with_environment("NANIKA_EXACT_ENV", "exact-environment")?
        .with_stdin(b"exact-stdin\n".to_vec())?;
    if let Some(block) = std::env::var_os(BLOCK) {
        request = request.with_environment("NANIKA_BLOCK_FILE", block.to_string_lossy())?;
    }
    Ok(request)
}

fn attempt(phase: &str, number: u32) -> TestResult<RustPilotProcessAttempt> {
    Ok(RustPilotProcessAttempt::new(
        MissionId::new("pilot-process-mission")?,
        phase,
        number,
    )?)
}

fn budget() -> ProcessBudget {
    ProcessBudget::new(
        Instant::now() + Duration::from_secs(10),
        Duration::from_secs(10),
        Duration::from_secs(5),
    )
}

fn open_ready(
    purpose: ProcessPurpose,
    phase: &str,
    number: u32,
    counter: &Path,
) -> TestResult<(RustPilotProcessSession, ProcessRequest, PathBuf)> {
    let writer = acquire()?;
    let cwd = ensure_cwd()?;
    let executable = RustPilotProcessExecutable::open(&child_path(EXECUTABLE)?)?;
    let request = request(purpose, executable.logical_id(), &cwd, counter)?;
    let opened = RustPilotProcessSession::open(
        &writer,
        attempt(phase, number)?,
        &request,
        executable,
        PathBuf::from("private-cwd"),
    )?;
    let RustPilotProcessOpen::Ready(session) = opened else {
        return Err("expected a dispatchable process session".into());
    };
    Ok((*session, request, cwd))
}

fn admission_snapshot(database: &Path, phase: &str) -> TestResult<(String, i64, i64, i64)> {
    let connection = Connection::open(database)?;
    let snapshot = connection.query_row(
        "SELECT journal.committed_at_utc,
                CAST(strftime('%s', journal.committed_at_utc) AS INTEGER),
                (SELECT count(*) FROM journal WHERE transition_kind = 'rust_pilot_process_admitted'),
                (SELECT count(*) FROM outbox)
         FROM journal
         WHERE transition_kind = 'rust_pilot_process_admitted'
           AND json_extract(payload_json, '$.phase_id') = ?1",
        [phase],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    connection.close().map_err(|(_, error)| error)?;
    Ok(snapshot)
}

fn epoch_seconds() -> TestResult<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    )?)
}

fn open_with_writer(
    writer: &orchestrator_app::ProductionWriterAuthority,
    purpose: ProcessPurpose,
    phase: &str,
    number: u32,
    counter: &Path,
) -> TestResult<(RustPilotProcessSession, ProcessRequest)> {
    let cwd = ensure_cwd()?;
    let executable = RustPilotProcessExecutable::open(&child_path(EXECUTABLE)?)?;
    let request = request(purpose, executable.logical_id(), &cwd, counter)?;
    let opened = RustPilotProcessSession::open(
        writer,
        attempt(phase, number)?,
        &request,
        executable,
        PathBuf::from("private-cwd"),
    )?;
    let RustPilotProcessOpen::Ready(session) = opened else {
        return Err("expected a dispatchable process session".into());
    };
    Ok((*session, request))
}

fn assert_success(output: &Output) -> TestResult {
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "child failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}

#[test]
fn child_helper() -> TestResult {
    let Some(action) = std::env::var_os(ACTION) else {
        return Ok(());
    };
    match action.to_str().ok_or("non-UTF-8 action")? {
        "system-executable" => {
            let writer = acquire()?;
            let cwd = ensure_cwd()?;
            let executable_path = fs::canonicalize("/usr/bin/printf")?;
            assert_eq!(fs::metadata(&executable_path)?.uid(), 0);
            let executable = RustPilotProcessExecutable::open(&executable_path)?;
            let request =
                ProcessRequest::new(ProcessPurpose::Verification, executable.logical_id(), &cwd)?
                    .with_argument("system-owned-verifier\n")?;
            let opened = RustPilotProcessSession::open(
                &writer,
                attempt("system-executable", 1)?,
                &request,
                executable,
                PathBuf::from("private-cwd"),
            )?;
            let RustPilotProcessOpen::Ready(session) = opened else {
                return Err("expected a fresh system executable admission".into());
            };
            let execution = session.execute(&request, budget())?;
            assert!(execution.receipt().is_success());
            assert_eq!(
                execution.receipt().expose_stdout(),
                b"system-owned-verifier\n"
            );
            let identity = execution
                .post_release_identity()
                .ok_or("missing system identity")?;
            let absence = execution.group_absence().ok_or("missing system cleanup")?;
            assert_eq!(identity.pid(), absence.pid());
            assert_eq!(identity.process_group_id(), absence.process_group_id());
            assert_eq!(
                identity.process_start_identity(),
                absence.process_start_identity()
            );
        }
        "success" => {
            for (purpose, phase, number) in [
                (ProcessPurpose::ProviderWorker, "provider", 1),
                (ProcessPurpose::Verification, "verification", 2),
            ] {
                let counter = child_path(TARGET)?.join(format!("{phase}.counter"));
                let (session, request, cwd) = open_ready(purpose, phase, number, &counter)?;
                let execution = session.execute(&request, budget())?;
                assert!(execution.receipt().is_success());
                assert!(matches!(
                    execution.receipt().termination(),
                    ProcessTerminationReceipt::Exited(status) if status.as_code() == Some(0)
                ));
                assert_eq!(
                    execution.receipt().expose_stdout(),
                    format!(
                        "stdout:exact-argv:exact-environment:exact-stdin:{}\n",
                        cwd.display()
                    )
                    .as_bytes()
                );
                assert_eq!(execution.receipt().expose_stderr(), b"stderr:exact-error\n");
                let identity = execution
                    .post_release_identity()
                    .ok_or("missing post-release kernel identity")?;
                let absence = execution
                    .group_absence()
                    .ok_or("missing exact process-group absence")?;
                assert_eq!(identity.pid(), absence.pid());
                assert_eq!(identity.process_group_id(), absence.process_group_id());
                assert_eq!(
                    identity.process_start_identity(),
                    absence.process_start_identity()
                );
                assert_eq!(fs::read(&counter)?, b"x");
            }
        }
        "mismatch-and-release" => {
            let counter = child_path(TARGET)?.join("mismatch.counter");
            let writer = acquire()?;
            let competing = acquire();
            assert!(competing.is_err());
            let cwd = ensure_cwd()?;
            let executable = RustPilotProcessExecutable::open(&child_path(EXECUTABLE)?)?;
            let wrong_executable = request(
                ProcessPurpose::ProviderWorker,
                "wrong-executable",
                &cwd,
                &counter,
            )?;
            assert!(matches!(
                RustPilotProcessSession::open(
                    &writer,
                    attempt("mismatch", 1)?,
                    &wrong_executable,
                    executable,
                    PathBuf::from("private-cwd")
                ),
                Err(RustPilotProcessError::RequestMismatch)
            ));
            let executable = RustPilotProcessExecutable::open(&child_path(EXECUTABLE)?)?;
            let wrong_cwd = request(
                ProcessPurpose::ProviderWorker,
                executable.logical_id(),
                &child_path(TARGET)?,
                &counter,
            )?;
            assert!(matches!(
                RustPilotProcessSession::open(
                    &writer,
                    attempt("mismatch", 2)?,
                    &wrong_cwd,
                    executable,
                    PathBuf::from("private-cwd")
                ),
                Err(RustPilotProcessError::RequestMismatch)
            ));
            let executable = RustPilotProcessExecutable::open(&child_path(EXECUTABLE)?)?;
            let unsupported = request(
                ProcessPurpose::Tool,
                executable.logical_id(),
                &cwd,
                &counter,
            )?;
            assert!(matches!(
                RustPilotProcessSession::open(
                    &writer,
                    attempt("mismatch", 3)?,
                    &unsupported,
                    executable,
                    PathBuf::from("private-cwd")
                ),
                Err(RustPilotProcessError::UnsupportedPurpose)
            ));
            assert!(acquire().is_err());
            assert!(!counter.exists());
            drop(writer);
            drop(acquire()?);
        }
        "overlap" => {
            let first_counter = child_path(TARGET)?.join("overlap-first.counter");
            let (session, _, _) = open_ready(
                ProcessPurpose::ProviderWorker,
                "overlap-first",
                1,
                &first_counter,
            )?;
            session.close()?;
            let second_counter = child_path(TARGET)?.join("overlap-second.counter");
            let writer = acquire()?;
            let cwd = ensure_cwd()?;
            let executable = RustPilotProcessExecutable::open(&child_path(EXECUTABLE)?)?;
            let request = request(
                ProcessPurpose::Verification,
                executable.logical_id(),
                &cwd,
                &second_counter,
            )?;
            assert!(matches!(
                RustPilotProcessSession::open(
                    &writer,
                    attempt("overlap-second", 1)?,
                    &request,
                    executable,
                    PathBuf::from("private-cwd")
                ),
                Err(RustPilotProcessError::ForeignProcessState)
            ));
            assert!(!first_counter.exists());
            assert!(!second_counter.exists());
            assert!(acquire().is_err());
            drop(writer);
            drop(acquire()?);
        }
        "request-mismatch" => {
            let counter = child_path(TARGET)?.join("request-mismatch.counter");
            let (session, admitted, cwd) = open_ready(
                ProcessPurpose::ProviderWorker,
                "request-mismatch",
                1,
                &counter,
            )?;
            let changed = request(
                ProcessPurpose::Verification,
                admitted.executable_id(),
                &cwd,
                &counter,
            )?;
            assert!(matches!(
                session.execute(&changed, budget()),
                Err(RustPilotProcessError::DispatchFailed)
            ));
            assert!(!counter.exists());
            drop(acquire()?);
        }
        "ownership-error" => {
            let counter = child_path(TARGET)?.join("ownership-error.counter");
            let (session, admitted, _) = open_ready(
                ProcessPurpose::ProviderWorker,
                "ownership-error",
                1,
                &counter,
            )?;
            assert!(matches!(
                session.execute(&admitted, budget()),
                Err(RustPilotProcessError::OwnershipJournal)
            ));
            assert!(!counter.exists());
            let ownership = child_path(TARGET)?.join(RUST_PILOT_PROCESS_OWNERSHIP_FILE);
            fs::remove_file(ownership)?;
            drop(acquire()?);
        }
        "close-reopen" => {
            let counter = child_path(TARGET)?.join("close.counter");
            let before = epoch_seconds()?;
            let (session, _, _) =
                open_ready(ProcessPurpose::ProviderWorker, "close-reopen", 1, &counter)?;
            session.close()?;
            let database = child_path(TARGET)?.join("runtime.db");
            let first = admission_snapshot(&database, "close-reopen")?;
            let (session, admitted, _) =
                open_ready(ProcessPurpose::ProviderWorker, "close-reopen", 1, &counter)?;
            session.execute(&admitted, budget())?;
            let reopened = admission_snapshot(&database, "close-reopen")?;
            let after = epoch_seconds()?;
            assert_eq!(first, reopened);
            assert!(first.1 >= before && first.1 <= after);
            assert_eq!((first.2, first.3), (1, 1));
            assert_eq!(fs::read(&counter)?, b"x");
            let writer = acquire()?;
            let cwd = ensure_cwd()?;
            let executable = RustPilotProcessExecutable::open(&child_path(EXECUTABLE)?)?;
            let request = request(
                ProcessPurpose::ProviderWorker,
                executable.logical_id(),
                &cwd,
                &counter,
            )?;
            assert!(matches!(
                RustPilotProcessSession::open(
                    &writer,
                    attempt("close-reopen", 1)?,
                    &request,
                    executable,
                    PathBuf::from("private-cwd")
                )?,
                RustPilotProcessOpen::Recovered(RustPilotProcessRecovery::PreviouslyResolved)
            ));
            assert_eq!(admission_snapshot(&database, "close-reopen")?, first);
            assert_eq!(fs::read(&counter)?, b"x");
        }
        "executable-binding-conflict" => {
            let counter = child_path(TARGET)?.join("executable-binding-conflict.counter");
            let writer = acquire()?;
            let cwd = ensure_cwd()?;
            let executable_a_path = child_path(EXECUTABLE)?;
            let executable_a = RustPilotProcessExecutable::open(&executable_a_path)?;
            let request_a = request(
                ProcessPurpose::ProviderWorker,
                executable_a.logical_id(),
                &cwd,
                &counter,
            )?;
            let opened = RustPilotProcessSession::open(
                &writer,
                attempt("executable-binding-conflict", 1)?,
                &request_a,
                executable_a,
                PathBuf::from("private-cwd"),
            )?;
            let RustPilotProcessOpen::Ready(session) = opened else {
                return Err("expected the original executable to be dispatchable".into());
            };
            session.close()?;

            let database = child_path(TARGET)?.join("runtime.db");
            let original_admission = admission_snapshot(&database, "executable-binding-conflict")?;
            let executable_b_path = executable_a_path.with_file_name("same-bytes-target.sh");
            private_file(&executable_b_path, &fs::read(&executable_a_path)?, 0o700)?;
            let executable_b_path = fs::canonicalize(executable_b_path)?;
            let executable_b = RustPilotProcessExecutable::open(&executable_b_path)?;
            let request_b = request(
                ProcessPurpose::ProviderWorker,
                executable_b.logical_id(),
                &cwd,
                &counter,
            )?;
            let conflict = match RustPilotProcessSession::open(
                &writer,
                attempt("executable-binding-conflict", 1)?,
                &request_b,
                executable_b,
                PathBuf::from("private-cwd"),
            ) {
                Err(error) => error,
                Ok(_) => return Err("replacement executable was unexpectedly admitted".into()),
            };
            assert!(
                matches!(conflict, RustPilotProcessError::ForeignProcessState),
                "unexpected executable admission error: {conflict:?}"
            );
            assert_eq!(
                admission_snapshot(&database, "executable-binding-conflict")?,
                original_admission
            );
            assert!(!counter.exists());

            let executable_a = RustPilotProcessExecutable::open(&executable_a_path)?;
            let reopened = RustPilotProcessSession::open(
                &writer,
                attempt("executable-binding-conflict", 1)?,
                &request_a,
                executable_a,
                PathBuf::from("private-cwd"),
            )?;
            let RustPilotProcessOpen::Ready(session) = reopened else {
                return Err("expected the original admitted executable to remain usable".into());
            };
            session.execute(&request_a, budget())?;
            assert_eq!(fs::read(&counter)?, b"x");
        }
        "sequential-writer" => {
            let writer = acquire()?;
            let retained_boundary = writer.boundary();
            let journal = child_path(TARGET)?.join("journal");
            fs::create_dir(&journal)?;
            private_directory(&journal)?;
            append_mission_journal_record(
                &writer,
                &journal.join("000001.json"),
                b"phase-start-1\n",
            )?;
            assert!(acquire().is_err());

            let first_counter = child_path(TARGET)?.join("sequential-first.counter");
            let (session, request) = open_with_writer(
                &writer,
                ProcessPurpose::ProviderWorker,
                "sequential-first",
                1,
                &first_counter,
            )?;
            session.execute(&request, budget())?;
            append_mission_journal_record(
                &writer,
                &journal.join("000002.json"),
                b"phase-terminal-1\n",
            )?;
            assert!(acquire().is_err());

            append_mission_journal_record(
                &writer,
                &journal.join("000003.json"),
                b"phase-start-2\n",
            )?;
            let second_counter = child_path(TARGET)?.join("sequential-second.counter");
            let (session, request) = open_with_writer(
                &writer,
                ProcessPurpose::Verification,
                "sequential-second",
                1,
                &second_counter,
            )?;
            session.execute(&request, budget())?;
            append_mission_journal_record(
                &writer,
                &journal.join("000004.json"),
                b"phase-terminal-2\n",
            )?;
            assert!(acquire().is_err());
            assert_eq!(fs::read(first_counter)?, b"x");
            assert_eq!(fs::read(second_counter)?, b"x");

            drop(writer);
            assert!(acquire().is_err());
            drop(retained_boundary);
            drop(acquire()?);
        }
        "crash-before" => {
            let counter = child_path(TARGET)?.join("crash-before.counter");
            let (session, request, _) =
                open_ready(ProcessPurpose::ProviderWorker, "crash-before", 1, &counter)?;
            let _ = session.execute(&request, budget());
            return Err("claimed barrier unexpectedly returned".into());
        }
        "recover-before" => {
            let counter = child_path(TARGET)?.join("crash-before.counter");
            let writer = acquire()?;
            let cwd = ensure_cwd()?;
            let executable = RustPilotProcessExecutable::open(&child_path(EXECUTABLE)?)?;
            let request = request(
                ProcessPurpose::ProviderWorker,
                executable.logical_id(),
                &cwd,
                &counter,
            )?;
            assert!(matches!(
                RustPilotProcessSession::open(
                    &writer,
                    attempt("crash-before", 1)?,
                    &request,
                    executable,
                    PathBuf::from("private-cwd")
                )?,
                RustPilotProcessOpen::Recovered(
                    RustPilotProcessRecovery::RecoveredBeforeStartNotStarted
                )
            ));
            assert!(!counter.exists());
        }
        "crash-after" => {
            let counter = child_path(TARGET)?.join("crash-after.counter");
            let (session, request, _) =
                open_ready(ProcessPurpose::Verification, "crash-after", 1, &counter)?;
            let _ = session.execute(&request, budget());
            return Err("released barrier unexpectedly returned".into());
        }
        "recover-after" => {
            let counter = child_path(TARGET)?.join("crash-after.counter");
            let writer = acquire()?;
            let cwd = ensure_cwd()?;
            let executable = RustPilotProcessExecutable::open(&child_path(EXECUTABLE)?)?;
            let request = request(
                ProcessPurpose::Verification,
                executable.logical_id(),
                &cwd,
                &counter,
            )?;
            assert!(matches!(
                RustPilotProcessSession::open(
                    &writer,
                    attempt("crash-after", 1)?,
                    &request,
                    executable,
                    PathBuf::from("private-cwd")
                )?,
                RustPilotProcessOpen::Recovered(
                    RustPilotProcessRecovery::ReleasedLostOutcomeUncertain
                )
            ));
            assert_eq!(fs::read(&counter)?, b"x");
        }
        "recover-mismatched-identity" => {
            let counter = child_path(TARGET)?.join("crash-after.counter");
            let writer = acquire()?;
            let cwd = ensure_cwd()?;
            let executable = RustPilotProcessExecutable::open(&child_path(EXECUTABLE)?)?;
            let request = request(
                ProcessPurpose::Verification,
                executable.logical_id(),
                &cwd,
                &counter,
            )?;
            match RustPilotProcessSession::open(
                &writer,
                attempt("crash-after", 1)?,
                &request,
                executable,
                PathBuf::from("private-cwd"),
            ) {
                Err(RustPilotProcessError::ForeignProcessState)
                | Err(RustPilotProcessError::RecoveryUnresolved)
                | Err(RustPilotProcessError::Store(RuntimeStoreError::CorruptDatabase)) => {}
                Err(error) => return Err(format!("unexpected recovery error: {error:?}").into()),
                Ok(_) => return Err("mismatched recovery identity was admitted".into()),
            }
            assert_eq!(fs::read(&counter)?, b"x");
        }
        _ => return Err("unknown child action".into()),
    }
    Ok(())
}

#[test]
fn protected_system_executable_runs_with_exact_process_cleanup() -> TestResult {
    let case = Case::new("system-executable")?;
    assert_success(&case.run("system-executable")?)
}

#[test]
fn writable_or_privileged_executable_is_still_refused() -> TestResult {
    let case = Case::new("unsafe-mode")?;
    for mode in [0o720, 0o702, 0o4700, 0o2700] {
        fs::set_permissions(&case.executable, fs::Permissions::from_mode(mode))?;
        assert!(matches!(
            RustPilotProcessExecutable::open(&case.executable),
            Err(RustPilotProcessError::ExecutableAdmission)
        ));
    }
    Ok(())
}

#[test]
fn provider_and_verifier_use_original_process_inputs_and_return_cleanup_proof() -> TestResult {
    let case = Case::new("success")?;
    assert_success(&case.run("success")?)
}

#[test]
fn request_executable_and_cwd_mismatches_do_not_launch_and_release_writer() -> TestResult {
    let case = Case::new("mismatch")?;
    assert_success(&case.run("mismatch-and-release")?)
}

#[test]
fn post_open_request_mismatch_is_refused_before_launch() -> TestResult {
    let case = Case::new("request-mismatch")?;
    assert_success(&case.run("request-mismatch")?)
}

#[test]
fn overlapping_private_process_attempt_is_refused_without_launch() -> TestResult {
    let case = Case::new("overlap")?;
    assert_success(&case.run("overlap")?)
}

#[test]
fn ownership_persistence_shape_error_prevents_launch_and_releases_writer() -> TestResult {
    let case = Case::new("ownership-error")?;
    assert_success(&case.fail_ownership_persistence()?)
}

#[test]
fn clean_close_and_used_home_reopen_do_not_dispatch_twice() -> TestResult {
    let case = Case::new("close-reopen")?;
    assert_success(&case.run("close-reopen")?)
}

#[test]
fn executable_ids_bind_path_and_file_identity_while_stable_reopens_match() -> TestResult {
    let case = Case::new("executable-ids")?;
    let bytes = fs::read(&case.executable)?;
    let other_path = case.parent.join("same-bytes-other-path.sh");
    private_file(&other_path, &bytes, 0o700)?;
    let other_path = fs::canonicalize(other_path)?;

    let original = RustPilotProcessExecutable::open(&case.executable)?;
    let reopened = RustPilotProcessExecutable::open(&case.executable)?;
    let other = RustPilotProcessExecutable::open(&other_path)?;
    assert_eq!(original.logical_id(), reopened.logical_id());
    assert_ne!(original.logical_id(), other.logical_id());
    assert_eq!(original.attestation(), other.attestation());

    fs::remove_file(&case.executable)?;
    private_file(&case.executable, &bytes, 0o700)?;
    let replacement_path = fs::canonicalize(&case.executable)?;
    assert_eq!(replacement_path, case.executable);
    let replacement = RustPilotProcessExecutable::open(&replacement_path)?;
    assert_ne!(original.logical_id(), replacement.logical_id());
    assert_eq!(original.attestation(), replacement.attestation());
    Ok(())
}

#[test]
fn pending_attempt_rejects_same_bytes_at_another_path_and_original_remains_usable() -> TestResult {
    let case = Case::new("executable-binding-conflict")?;
    assert_success(&case.run("executable-binding-conflict")?)
}

#[test]
fn one_writer_supports_two_attempts_and_mission_journal_writes_without_a_lease_gap() -> TestResult {
    let case = Case::new("sequential-writer")?;
    assert_success(&case.run("sequential-writer")?)
}

#[test]
fn kill_restart_before_release_is_not_started_and_never_executes_target() -> TestResult {
    let case = Case::new("crash-before")?;
    case.crash_at("claimed", "crash-before")?;
    assert_success(&case.run("recover-before")?)
}

#[test]
fn kill_restart_after_release_is_uncertain_and_never_executes_twice() -> TestResult {
    let case = Case::new("crash-after")?;
    case.crash_at("started", "crash-after")?;
    let output = case
        .command("recover-after")?
        .env(BLOCK, case.parent.join("started.block"))
        .output()?;
    assert_success(&output)
}

#[test]
fn mismatched_identity_refuses_recovery_and_never_signals_an_unowned_group() -> TestResult {
    let case = Case::new("identity-mismatch")?;
    case.crash_at("started", "crash-after")?;

    let mut sentinel_command = Command::new("/bin/sh");
    sentinel_command
        .args(["-c", "while :; do sleep 1; done"])
        .process_group(0);
    let mut sentinel = sentinel_command.spawn()?;
    let sentinel_identity = KernelProcessIdentity::observe(sentinel.id(), sentinel.id())?;
    let database = case.target.join("runtime.db");
    let original_group =
        replace_recorded_process_group(&database, sentinel_identity.process_group_id())?;
    let mismatch = case
        .command("recover-mismatched-identity")?
        .env(BLOCK, case.parent.join("started.block"))
        .output()?;
    assert_success(&mismatch)?;
    assert!(sentinel.try_wait()?.is_none());

    replace_recorded_process_group(&database, original_group)?;
    let recovered = case
        .command("recover-after")?
        .env(BLOCK, case.parent.join("started.block"))
        .output()?;
    assert_success(&recovered)?;
    sentinel.kill()?;
    let _ = sentinel.wait()?;
    Ok(())
}
