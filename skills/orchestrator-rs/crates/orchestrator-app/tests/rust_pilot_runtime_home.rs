#![cfg(unix)]

use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use orchestrator_app::{ApplicationError, IsolatedHomeGuards, RustPilotRuntimeHome};

const ACTION: &str = "NANIKA_PILOT_TEST_ACTION";
const TARGET: &str = "NANIKA_PILOT_TEST_TARGET";
const GUARD_HOME: &str = "NANIKA_PILOT_TEST_GUARD_HOME";
const CHECKOUT: &str = "NANIKA_PILOT_TEST_CHECKOUT";
const READY: &str = "NANIKA_PILOT_TEST_READY";
const RELEASE: &str = "NANIKA_PILOT_TEST_RELEASE";
const VERSION: &str = "NANIKA_PILOT_TEST_VERSION";
const PILOT: &str = "NANIKA_RUST_FIRST_USE_PILOT";
const LIVE: &str = "NANIKA_LIVE_HOME_ENROLL";
const ISOLATED: &str = "NANIKA_ISOLATED_HOME_ENROLL";
const SEAL: &str = "orchestrator.rust-pilot.seal";
const LOCK: &str = "orchestrator.writer.lock";
const TEST_VERSION: &str = "integration-test-v1";

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    parent: PathBuf,
    actual_home: PathBuf,
    guard_home: PathBuf,
    checkout: PathBuf,
    target: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        let suffix = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let temp = fs::canonicalize(std::env::temp_dir())?;
        let parent = temp.join(format!(
            "orchestrator-rust-pilot-{label}-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir(&parent)?;
        make_private_directory(&parent)?;
        let actual_home = parent.join("actual-home");
        let guard_home = parent.join("guard-home");
        let checkout = parent.join("checkout");
        for directory in [&actual_home, &guard_home, &checkout] {
            fs::create_dir(directory)?;
            make_private_directory(directory)?;
        }
        let target = parent.join("pilot-home");
        Ok(Self {
            parent,
            actual_home,
            guard_home,
            checkout,
            target,
        })
    }

    fn command(&self, action: &str) -> TestResult<Command> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--exact", "child_helper", "--nocapture"])
            .env_remove(ACTION)
            .env_remove(TARGET)
            .env_remove(GUARD_HOME)
            .env_remove(CHECKOUT)
            .env_remove(READY)
            .env_remove(RELEASE)
            .env_remove(VERSION)
            .env_remove(PILOT)
            .env_remove(LIVE)
            .env_remove(ISOLATED)
            .env("HOME", &self.actual_home)
            .env(ACTION, action)
            .env(TARGET, &self.target)
            .env(GUARD_HOME, &self.guard_home)
            .env(CHECKOUT, &self.checkout)
            .env(VERSION, TEST_VERSION)
            .env(PILOT, "1");
        Ok(command)
    }

    fn run(&self, action: &str) -> TestResult<Output> {
        Ok(self.command(action)?.output()?)
    }

    fn spawn_holder(&self, action: &str) -> TestResult<Holder> {
        let ready = self.parent.join("holder.ready");
        let release = self.parent.join("holder.release");
        let mut command = self.command(action)?;
        command
            .env(READY, &ready)
            .env(RELEASE, &release)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn()?;
        let mut holder = Holder {
            child: Some(child),
            ready,
            release,
        };
        holder.wait_until_ready()?;
        Ok(holder)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

struct Holder {
    child: Option<Child>,
    ready: PathBuf,
    release: PathBuf,
}

impl Holder {
    fn wait_until_ready(&mut self) -> TestResult {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.ready.is_file() {
                return Ok(());
            }
            if let Some(status) = self.child_mut()?.try_wait()? {
                return Err(format!("holder exited before readiness with {status}").into());
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err("holder did not signal readiness".into())
    }

    fn finish(mut self) -> TestResult {
        create_private_file(&self.release, b"release\n")?;
        let output = self
            .child
            .take()
            .ok_or("holder child missing")?
            .wait_with_output()?;
        assert_success(&output)
    }

    fn child_mut(&mut self) -> TestResult<&mut Child> {
        self.child
            .as_mut()
            .ok_or_else(|| "holder child missing".into())
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn make_private_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn create_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn child_path(name: &str) -> TestResult<PathBuf> {
    Ok(PathBuf::from(std::env::var_os(name).ok_or(name)?))
}

fn child_acquire() -> TestResult<orchestrator_app::ProductionWriterAuthority> {
    let target = child_path(TARGET)?;
    let guards = IsolatedHomeGuards {
        user_home: child_path(GUARD_HOME)?,
        repository_checkout: child_path(CHECKOUT)?,
    };
    let version = std::env::var(VERSION)?;
    Ok(RustPilotRuntimeHome::acquire(&target, &guards, version)?)
}

fn wait_for_release() -> TestResult {
    let release = child_path(RELEASE)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if release.is_file() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err("release was not signalled".into())
}

fn signal_ready() -> TestResult {
    create_private_file(&child_path(READY)?, b"ready\n")?;
    Ok(())
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

fn create_sealed_home(fixture: &Fixture) -> TestResult {
    fs::create_dir(&fixture.target)?;
    make_private_directory(&fixture.target)?;
    create_private_file(
        &fixture.target.join(SEAL),
        format!("nanika-rust-pilot-v1\n{TEST_VERSION}\n").as_bytes(),
    )?;
    Ok(())
}

fn entry_names(path: &Path) -> TestResult<Vec<String>> {
    let mut names = fs::read_dir(path)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>, std::io::Error>>()?;
    names.sort();
    Ok(names)
}

#[test]
fn child_helper() -> TestResult {
    let Some(action) = std::env::var_os(ACTION) else {
        return Ok(());
    };
    match action.to_str().ok_or("child action is not UTF-8")? {
        "acquire-ok" => {
            let authority = child_acquire()?;
            authority.boundary().verify()?;
        }
        "acquire-refused" => {
            let result = child_acquire();
            assert!(matches!(
                result,
                Err(error) if matches!(error.downcast_ref::<ApplicationError>(), Some(ApplicationError::RustPilotAdmissionRefused))
            ));
        }
        "not-enabled" => {
            let result = child_acquire();
            assert!(matches!(
                result,
                Err(error) if matches!(error.downcast_ref::<ApplicationError>(), Some(ApplicationError::RustPilotNotEnabled))
            ));
        }
        "hold-authority" => {
            let _authority = child_acquire()?;
            signal_ready()?;
            wait_for_release()?;
        }
        "write-state" => {
            let _authority = child_acquire()?;
            let target = child_path(TARGET)?;
            create_private_file(&target.join("private-state"), b"pilot state bytes\n")?;
            let directory = target.join("private-state-dir");
            fs::create_dir(&directory)?;
            make_private_directory(&directory)?;
            create_private_file(&directory.join("nested"), b"nested bytes\n")?;
        }
        "hold-boundary" => {
            let authority = child_acquire()?;
            let boundary = authority.boundary();
            drop(authority);
            signal_ready()?;
            wait_for_release()?;
            boundary.verify()?;
        }
        "boundary-must-fail" => {
            let authority = child_acquire()?;
            let boundary = authority.boundary();
            drop(authority);
            signal_ready()?;
            wait_for_release()?;
            assert!(boundary.verify().is_err());
        }
        other => return Err(format!("unknown child action {other}").into()),
    }
    Ok(())
}

#[test]
fn fresh_create_and_exactly_sealed_reopen_succeed() -> TestResult {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let fixture = Fixture::new("create-reopen")?;
    assert_success(&fixture.run("acquire-ok")?)?;
    assert_success(&fixture.run("acquire-ok")?)?;

    assert_eq!(
        fs::read(fixture.target.join(SEAL))?,
        format!("nanika-rust-pilot-v1\n{TEST_VERSION}\n").as_bytes()
    );
    assert_eq!(
        fs::metadata(&fixture.target)?.permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(fixture.target.join(SEAL))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(fs::metadata(fixture.target.join(SEAL))?.nlink(), 1);
    Ok(())
}

#[test]
fn subprocess_reopen_preserves_private_run_artifacts() -> TestResult {
    let fixture = Fixture::new("reopen-artifacts")?;
    assert_success(&fixture.run("write-state")?)?;
    let state = fixture.target.join("private-state");
    let nested = fixture.target.join("private-state-dir/nested");
    let state_bytes = fs::read(&state)?;
    let nested_bytes = fs::read(&nested)?;
    assert_success(&fixture.run("acquire-ok")?)?;
    assert_eq!(fs::read(state)?, state_bytes);
    assert_eq!(fs::read(nested)?, nested_bytes);
    Ok(())
}

#[test]
fn simultaneous_competing_process_is_refused() -> TestResult {
    let fixture = Fixture::new("competing-process")?;
    let holder = fixture.spawn_holder("hold-authority")?;

    assert_success(&fixture.run("acquire-refused")?)?;
    holder.finish()
}

#[test]
fn retained_boundary_clone_keeps_the_writer_lease() -> TestResult {
    let fixture = Fixture::new("retained-boundary")?;
    let holder = fixture.spawn_holder("hold-boundary")?;

    assert_success(&fixture.run("acquire-refused")?)?;
    holder.finish()
}

#[test]
fn missing_seal_refuses_without_mutating_unrelated_directory() -> TestResult {
    let fixture = Fixture::new("missing-seal")?;
    fs::create_dir(&fixture.target)?;
    make_private_directory(&fixture.target)?;
    create_private_file(&fixture.target.join("unrelated"), b"preserve me\n")?;
    let before = entry_names(&fixture.target)?;

    assert_success(&fixture.run("acquire-refused")?)?;

    assert_eq!(entry_names(&fixture.target)?, before);
    assert_eq!(
        fs::read(fixture.target.join("unrelated"))?,
        b"preserve me\n"
    );
    Ok(())
}

#[test]
fn wrong_seal_refuses_without_mutating_the_directory() -> TestResult {
    let fixture = Fixture::new("wrong-seal")?;
    fs::create_dir(&fixture.target)?;
    make_private_directory(&fixture.target)?;
    create_private_file(&fixture.target.join(SEAL), b"wrong seal\n")?;
    let before = entry_names(&fixture.target)?;

    assert_success(&fixture.run("acquire-refused")?)?;

    assert_eq!(entry_names(&fixture.target)?, before);
    assert_eq!(fs::read(fixture.target.join(SEAL))?, b"wrong seal\n");
    Ok(())
}

#[test]
fn originally_requested_final_symlink_is_refused() -> TestResult {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new("final-symlink")?;
    let destination = fixture.parent.join("destination");
    fs::create_dir(&destination)?;
    make_private_directory(&destination)?;
    symlink(&destination, &fixture.target)?;

    assert_success(&fixture.run("acquire-refused")?)?;
    assert!(entry_names(&destination)?.is_empty());
    Ok(())
}

#[test]
fn hardlinked_seal_is_refused() -> TestResult {
    let fixture = Fixture::new("hardlinked-seal")?;
    create_sealed_home(&fixture)?;
    fs::hard_link(
        fixture.target.join(SEAL),
        fixture.parent.join("second-seal-link"),
    )?;

    assert_success(&fixture.run("acquire-refused")?)
}

#[test]
fn nonprivate_seal_is_refused() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new("nonprivate-seal")?;
    create_sealed_home(&fixture)?;
    fs::set_permissions(fixture.target.join(SEAL), fs::Permissions::from_mode(0o644))?;

    assert_success(&fixture.run("acquire-refused")?)
}

#[test]
fn nonprivate_root_is_refused() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new("nonprivate-root")?;
    fs::create_dir(&fixture.target)?;
    fs::set_permissions(&fixture.target, fs::Permissions::from_mode(0o755))?;

    assert_success(&fixture.run("acquire-refused")?)?;
    assert!(entry_names(&fixture.target)?.is_empty());
    Ok(())
}

#[test]
fn changed_root_identity_invalidates_the_retained_boundary() -> TestResult {
    let fixture = Fixture::new("changed-root")?;
    let holder = fixture.spawn_holder("boundary-must-fail")?;
    fs::rename(&fixture.target, fixture.parent.join("displaced-root"))?;
    fs::create_dir(&fixture.target)?;
    make_private_directory(&fixture.target)?;

    holder.finish()
}

#[test]
fn changed_lock_identity_invalidates_the_retained_boundary() -> TestResult {
    let fixture = Fixture::new("changed-lock")?;
    let holder = fixture.spawn_holder("boundary-must-fail")?;
    fs::rename(
        fixture.target.join(LOCK),
        fixture.target.join("displaced-writer.lock"),
    )?;
    create_private_file(&fixture.target.join(LOCK), b"replacement\n")?;

    holder.finish()
}

#[test]
fn actual_home_alluka_is_refused_when_guard_home_differs() -> TestResult {
    let mut fixture = Fixture::new("actual-home-alluka")?;
    fixture.target = fixture.actual_home.join(".alluka");

    assert_success(&fixture.run("acquire-refused")?)?;
    assert!(!fixture.target.exists());
    Ok(())
}

#[test]
fn missing_pilot_opt_in_is_refused() -> TestResult {
    let fixture = Fixture::new("missing-opt-in")?;
    let mut command = fixture.command("not-enabled")?;
    command.env_remove(PILOT);

    assert_success(&command.output()?)?;
    assert!(!fixture.target.exists());
    Ok(())
}

#[test]
fn nonexact_pilot_opt_in_is_refused() -> TestResult {
    let fixture = Fixture::new("nonexact-opt-in")?;
    let mut command = fixture.command("not-enabled")?;
    command.env(PILOT, "true");

    assert_success(&command.output()?)?;
    assert!(!fixture.target.exists());
    Ok(())
}

#[test]
fn live_selector_is_refused_even_with_pilot_opt_in() -> TestResult {
    let fixture = Fixture::new("live-selector")?;
    let mut command = fixture.command("not-enabled")?;
    command.env(LIVE, "1");

    assert_success(&command.output()?)?;
    assert!(!fixture.target.exists());
    Ok(())
}

#[test]
fn isolated_selector_is_refused_even_with_pilot_opt_in() -> TestResult {
    let fixture = Fixture::new("isolated-selector")?;
    let mut command = fixture.command("not-enabled")?;
    command.env(ISOLATED, "1");

    assert_success(&command.output()?)?;
    assert!(!fixture.target.exists());
    Ok(())
}
