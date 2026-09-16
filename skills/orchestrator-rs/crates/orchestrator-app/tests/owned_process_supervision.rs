use orchestrator_app::{
    CancellationToken, FixtureAdmissionPolicy, FixtureProcessAuthority, FixtureProcessReport,
    FixtureProcessSpec, FixtureWorkspaceSeed, FreshFixtureAuthority, IsolatedFixtureRoot,
    ProcessError, ProcessTermination, SupervisorError, SupervisorLimits,
};
use orchestrator_core::{CheckpointProjection, MissionId, VerificationClass, VerificationOutcome};
use rustix::{
    io::{FdFlags, fcntl_setfd},
    process::{Pid, test_kill_process, test_kill_process_group},
};
use std::{
    fs::{File, OpenOptions},
    io::{Seek, Write},
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
    time::{Duration, Instant},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static PROCESS_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct FixtureCase {
    _guard: MutexGuard<'static, ()>,
    parent: PathBuf,
    root: PathBuf,
    authority: Option<FixtureProcessAuthority>,
}

impl Drop for FixtureCase {
    fn drop(&mut self) {
        self.authority.take();
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

fn fixture_case(label: &str) -> TestResult<FixtureCase> {
    let limits =
        SupervisorLimits::for_tests(Duration::from_millis(80), Duration::from_millis(500))?;
    fixture_case_with_limits(label, limits)
}

fn fixture_case_with_limits(label: &str, limits: SupervisorLimits) -> TestResult<FixtureCase> {
    let guard = match PROCESS_TEST_LOCK.get_or_init(|| Mutex::new(())).lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let canonical_temp = std::fs::canonicalize(std::env::temp_dir())?;
    let parent = canonical_temp.join(format!(
        "orchestrator-owned-process-{}-{label}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&parent);
    private_dir(&parent)?;
    let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
    let root = isolated.path().to_path_buf();
    let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
    let helper_bytes = std::fs::read(env!("CARGO_BIN_EXE_orchestrator-owned-process-fixture"))?;
    let policy = FixtureAdmissionPolicy::new(parent.join("live-user"), checkout, &canonical_temp)
        .with_expected_fixture_helper(&helper_bytes);
    let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
    let checkpoint = CheckpointProjection {
        workspace_id: "owned-process-case".to_owned(),
        status: "pending".to_owned(),
        started_at: "2026-07-13T00:00:00Z".to_owned(),
        ..CheckpointProjection::default()
    };
    let workspace = authority.create_workspace(
        MissionId::new("owned-process-case")?,
        FixtureWorkspaceSeed::new(b"fixture\n".to_vec(), &checkpoint, b"{}".to_vec())?,
    )?;
    let executable = authority.install_fixture_executable("native-helper", &helper_bytes)?;
    let process_authority = FixtureProcessAuthority::new(executable, &workspace, limits)?;
    Ok(FixtureCase {
        _guard: guard,
        parent,
        root,
        authority: Some(process_authority),
    })
}

fn authority(case: &FixtureCase) -> TestResult<&FixtureProcessAuthority> {
    case.authority
        .as_ref()
        .ok_or_else(|| "missing authority".into())
}

fn spec(arguments: &[&str], timeout: Duration) -> TestResult<FixtureProcessSpec> {
    Ok(FixtureProcessSpec::new(arguments.iter().copied(), timeout)?)
}

fn assert_group_absent(pgid: Option<u32>) -> TestResult {
    let raw = pgid.ok_or("report omitted its process-group ID")?;
    let pid = Pid::from_raw(raw as i32).ok_or("invalid pgid")?;
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match test_kill_process_group(pid) {
            Err(rustix::io::Errno::SRCH) => return Ok(()),
            Ok(()) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(()) => return Err(format!("process group {raw} survived").into()),
            Err(error) => return Err(format!("cannot probe group {raw}: {error}").into()),
        }
    }
}

fn assert_cancelled_process_absent(report: &FixtureProcessReport) -> TestResult {
    if report.process.spawned {
        assert_group_absent(report.process.pgid)
    } else {
        assert!(report.process.pid.is_none() && report.process.pgid.is_none());
        Ok(())
    }
}

fn assert_pid_absent(raw: u32) -> TestResult {
    let pid = Pid::from_raw(raw as i32).ok_or("invalid pid")?;
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match test_kill_process(pid) {
            Err(rustix::io::Errno::SRCH) => return Ok(()),
            Ok(()) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(()) => return Err(format!("process {raw} survived").into()),
            Err(error) => return Err(format!("cannot probe process {raw}: {error}").into()),
        }
    }
}

fn descendant_pids(bytes: &[u8]) -> Vec<u32> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|line| line.strip_prefix("DESCENDANT "))
        .filter_map(|value| value.parse().ok())
        .collect()
}

fn fd_count() -> TestResult<usize> {
    Ok(std::fs::read_dir("/dev/fd")?.count())
}

fn wait_until_owned(authority: &FixtureProcessAuthority) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !authority.has_unresolved_processes() {
        if Instant::now() >= deadline {
            return Err("production supervisor did not register the fixture process".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

fn join_report(
    result: std::thread::Result<Result<FixtureProcessReport, SupervisorError>>,
) -> TestResult<FixtureProcessReport> {
    let result = result.map_err(|_| std::io::Error::other("fixture process thread panicked"))?;
    Ok(result?)
}

#[test]
fn passes_exact_argv_and_allowlisted_environment() -> TestResult {
    let case = fixture_case("argv-env")?;
    let report = authority(&case)?.run(&spec(
        &["inspect", "literal spaces", "$(not-a-shell)", "semi;colon"],
        Duration::from_secs(2),
    )?)?;
    assert!(report.is_success(), "{report:#?}");
    let line = report
        .process
        .stdout
        .split(|byte| *byte == b'\n')
        .next()
        .ok_or("missing inspection")?;
    let value: serde_json::Value = serde_json::from_slice(line)?;
    assert_eq!(
        value["arguments"],
        serde_json::json!(["inspect", "literal spaces", "$(not-a-shell)", "semi;colon"])
    );
    let environment = value["environment"]
        .as_object()
        .ok_or("missing environment")?;
    assert_eq!(environment.len(), 6);
    assert!(!environment.contains_key("ORCHESTRATOR_POISON"));
    assert!(
        value["cwd"]
            .as_str()
            .unwrap_or_default()
            .ends_with("workspaces/owned-process-case")
    );
    assert_group_absent(report.process.pgid)
}

#[test]
fn normal_nonzero_signal_missing_protocol_and_tampered_executable_table() -> TestResult {
    for (mode, expected, success) in [
        ("zero", Some(ProcessTermination::Exited(0)), true),
        ("nonzero", Some(ProcessTermination::Exited(7)), false),
        ("signal", None, false),
        ("missing-final", Some(ProcessTermination::Exited(0)), false),
    ] {
        let case = fixture_case(mode)?;
        let report = authority(&case)?.run(&spec(&[mode], Duration::from_secs(2))?)?;
        if let Some(expected) = expected {
            assert_eq!(report.process.termination, expected, "{report:#?}");
        } else {
            assert!(
                matches!(report.process.termination, ProcessTermination::Signaled(_)),
                "{report:#?}"
            );
        }
        assert_eq!(report.is_success(), success, "{report:#?}");
        assert!(report.process.direct_child_reaped && report.process.group_absent);
        assert_eq!(report.protocol.complete, mode != "missing-final");
        assert_group_absent(report.process.pgid)?;
    }

    let case = fixture_case("spawn-error")?;
    let mut altered = OpenOptions::new()
        .write(true)
        .open(case.root.join("bin/native-helper"))?;
    altered.seek(std::io::SeekFrom::Start(0))?;
    altered.write_all(&[0])?;
    altered.flush()?;
    assert!(matches!(
        authority(&case)?.run(&spec(&["zero"], Duration::from_secs(1))?),
        Err(SupervisorError::Capability)
    ));
    assert!(!authority(&case)?.has_unresolved_processes());
    Ok(())
}

#[test]
fn structured_verification_result_ignores_misleading_human_output() -> TestResult {
    for (mode, expected) in [
        (
            "verification-pass",
            VerificationOutcome::Classified(VerificationClass::Pass),
        ),
        (
            "verification-fail",
            VerificationOutcome::Classified(VerificationClass::Fail),
        ),
        (
            "verification-skip",
            VerificationOutcome::Classified(VerificationClass::Skip),
        ),
        (
            "verification-no-tests",
            VerificationOutcome::Classified(VerificationClass::NoTests),
        ),
        (
            "verification-misleading",
            VerificationOutcome::Classified(VerificationClass::Fail),
        ),
        (
            "verification-contradictory",
            VerificationOutcome::Classified(VerificationClass::InfrastructureError),
        ),
    ] {
        let case = fixture_case(mode)?;
        let report = authority(&case)?.run(&spec(&[mode], Duration::from_secs(2))?)?;
        assert_eq!(report.verification_outcome(), expected, "mode {mode}");
        assert_eq!(
            report.is_success(),
            expected == VerificationOutcome::Classified(VerificationClass::Pass),
            "mode {mode}: {report:#?}"
        );
        if mode == "verification-misleading" {
            assert!(String::from_utf8_lossy(&report.process.stdout).contains("PASS"));
        }
        assert!(report.process.direct_child_reaped && report.process.group_absent);
    }
    Ok(())
}

#[test]
fn cwd_alias_swap_is_denied_before_production_spawn() -> TestResult {
    let case = fixture_case("cwd-swap")?;
    let workspace = case.root.join("workspaces/owned-process-case");
    let held = case.root.join("workspaces/held-workspace");
    let outside = case.parent.join("outside");
    private_dir(&outside)?;
    std::fs::rename(&workspace, &held)?;
    symlink(&outside, &workspace)?;

    assert!(matches!(
        authority(&case)?.run(&spec(&["zero"], Duration::from_secs(1))?),
        Err(SupervisorError::Capability)
    ));
    assert!(!authority(&case)?.has_unresolved_processes());

    std::fs::remove_file(&workspace)?;
    std::fs::rename(&held, &workspace)?;
    Ok(())
}

#[test]
fn production_output_limit_drains_streams_and_retains_protocol() -> TestResult {
    let case = fixture_case("output")?;
    let fixture = spec(&["output"], Duration::from_secs(3))?.with_max_output_bytes(256 * 1024);
    let report = authority(&case)?.run(&fixture)?;
    assert_eq!(report.process.termination, ProcessTermination::Exited(0));
    assert!(report.process.truncated);
    assert_eq!(report.process.stdout.len(), 256 * 1024);
    assert!(report.process.stdout_discarded_bytes > 0);
    assert!(report.process.stderr.len() <= 256 * 1024);
    assert!(report.protocol.complete, "{report:#?}");
    assert!(!report.is_success());
    assert_group_absent(report.process.pgid)
}

#[test]
fn logical_descendants_are_protocol_balanced_and_reaped() -> TestResult {
    let case = fixture_case("tree")?;
    let report = authority(&case)?.run(&spec(&["tree"], Duration::from_secs(3))?)?;
    assert!(report.is_success(), "{report:#?}");
    assert_eq!(report.protocol.logical_children_started, 2);
    assert_eq!(report.protocol.logical_children_reaped, 2);
    for pid in descendant_pids(&report.process.stdout) {
        assert_pid_absent(pid)?;
    }
    assert_group_absent(report.process.pgid)
}

#[test]
fn timeout_reaps_an_entire_descendant_tree() -> TestResult {
    let case = fixture_case("kill-tree")?;
    let report = authority(&case)?.run(&spec(&["hang-tree"], Duration::from_millis(500))?)?;
    assert_eq!(report.process.termination, ProcessTermination::Timeout);
    assert!(report.process.term_sent, "{report:#?}");
    assert!(report.process.direct_child_reaped && report.process.group_absent);
    for pid in descendant_pids(&report.process.stdout) {
        assert_pid_absent(pid)?;
    }
    assert_group_absent(report.process.pgid)
}

#[cfg(target_os = "linux")]
#[test]
fn timeout_escalates_for_a_term_resistant_tree() -> TestResult {
    let case = fixture_case("kill-stopped-tree")?;
    let report = authority(&case)?.run(&spec(&["stop-tree"], Duration::from_millis(500))?)?;
    assert_eq!(report.process.termination, ProcessTermination::Timeout);
    assert!(
        report.process.term_sent && report.process.kill_sent,
        "{report:#?}"
    );
    assert!(report.process.direct_child_reaped && report.process.group_absent);
    for pid in descendant_pids(&report.process.stdout) {
        assert_pid_absent(pid)?;
    }
    assert_group_absent(report.process.pgid)
}

#[test]
fn silent_fixture_stalls_before_its_hard_deadline_and_cleans_up() -> TestResult {
    let case = fixture_case("silent-stall")?;
    let fixture =
        spec(&["sleep"], Duration::from_secs(2))?.with_stall_timeout(Duration::from_millis(75))?;
    let started = Instant::now();
    let report = authority(&case)?.run(&fixture)?;
    assert_eq!(report.process.termination, ProcessTermination::Stalled);
    assert!(report.process.stall_observed, "{report:#?}");
    assert!(started.elapsed() >= Duration::from_millis(50));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(report.process.direct_child_reaped && report.process.group_absent);
    assert_eq!(
        report.verification_outcome(),
        VerificationOutcome::Classified(VerificationClass::Timeout)
    );
    assert_group_absent(report.process.pgid)
}

#[test]
fn periodic_fixture_activity_resets_the_stall_deadline() -> TestResult {
    let case = fixture_case("periodic-activity")?;
    let fixture = spec(&["periodic"], Duration::from_secs(5))?
        .with_stall_timeout(Duration::from_millis(500))?;
    let report = authority(&case)?.run(&fixture)?;
    assert!(report.is_success(), "{report:#?}");
    assert_eq!(report.process.termination, ProcessTermination::Exited(0));
    assert!(!report.process.stall_observed, "{report:#?}");
    assert_group_absent(report.process.pgid)
}

#[test]
fn cancellation_token_wakes_and_reaps_without_polling() -> TestResult {
    let case = fixture_case("cancel")?;
    let process_authority = authority(&case)?;
    let token = CancellationToken::new();
    let fixture = spec(&["sleep"], Duration::from_secs(30))?.with_cancellation(token.clone());
    let report = std::thread::scope(|scope| {
        let handle = scope.spawn(|| process_authority.run(&fixture));
        wait_until_owned(process_authority)?;
        assert!(token.cancel());
        assert!(!token.cancel());
        join_report(handle.join())
    })?;
    assert_eq!(report.process.termination, ProcessTermination::Cancelled);
    assert!(report.process.cancellation_observed);
    assert!(report.process.cleanup_complete, "{report:#?}");
    assert!(!authority(&case)?.has_unresolved_processes());
    assert_cancelled_process_absent(&report)
}

#[test]
fn caller_unwind_can_drop_a_cancellation_guard_and_reap() -> TestResult {
    let case = fixture_case("unwind")?;
    let process_authority = authority(&case)?;
    let token = CancellationToken::new();
    let fixture = spec(&["sleep"], Duration::from_secs(30))?.with_cancellation(token.clone());
    let report = std::thread::scope(|scope| {
        let handle = scope.spawn(|| process_authority.run(&fixture));
        wait_until_owned(process_authority)?;
        let guard = CancelOnDrop(token);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = guard;
            std::panic::resume_unwind(Box::new("injected unwind"));
        }));
        assert!(unwind.is_err());
        join_report(handle.join())
    })?;
    assert_eq!(report.process.termination, ProcessTermination::Cancelled);
    assert!(report.process.cleanup_complete, "{report:#?}");
    assert!(!authority(&case)?.has_unresolved_processes());
    assert_cancelled_process_absent(&report)
}

#[test]
fn stdin_is_written_concurrently_without_deadlock() -> TestResult {
    let case = fixture_case("stdin")?;
    let payload = b"fixture-stdin\n".repeat(2048);
    let fixture = spec(&["stdin-race"], Duration::from_secs(2))?.with_stdin(payload);
    let report = authority(&case)?.run(&fixture)?;
    assert!(report.is_success(), "{report:#?}");
    assert!(report.process.cleanup_complete);
    assert_group_absent(report.process.pgid)
}

#[test]
fn ambient_descriptor_and_capacity_are_denied_before_leak_or_spawn() -> TestResult {
    let limits =
        SupervisorLimits::for_tests(Duration::from_millis(80), Duration::from_millis(500))?
            .with_max_owned_groups(1)?;
    let case = fixture_case_with_limits("descriptors-capacity", limits)?;
    let process_authority = authority(&case)?;
    let decoy_path = case.parent.join("descriptor-decoy");
    std::fs::write(&decoy_path, b"decoy")?;
    let decoy = File::open(&decoy_path)?;
    fcntl_setfd(&decoy, FdFlags::empty())?;
    let report = authority(&case)?.run(&spec(&["descriptors"], Duration::from_secs(2))?)?;
    let descriptors: Vec<(i32, String)> = serde_json::from_slice(
        report
            .process
            .stdout
            .split(|byte| *byte == b'\n')
            .next()
            .ok_or("missing fd inventory")?,
    )?;
    assert!(
        !descriptors
            .iter()
            .any(|(_, target)| target == decoy_path.to_string_lossy().as_ref()),
        "ambient descriptor leaked: {descriptors:?}"
    );

    let token = CancellationToken::new();
    let fixture = spec(&["sleep"], Duration::from_secs(30))?.with_cancellation(token.clone());
    std::thread::scope(|scope| -> TestResult {
        let handle = scope.spawn(|| process_authority.run(&fixture));
        wait_until_owned(process_authority)?;
        assert!(matches!(
            process_authority.run(&spec(&["zero"], Duration::from_secs(1))?),
            Err(SupervisorError::Process(ProcessError::Spawn(error)))
                if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        token.cancel();
        let cancelled = join_report(handle.join())?;
        assert_eq!(cancelled.process.termination, ProcessTermination::Cancelled);
        Ok(())
    })?;
    assert!(!authority(&case)?.has_unresolved_processes());
    Ok(())
}

#[test]
fn hundred_fixture_cycles_return_process_resources_to_baseline() -> TestResult {
    let case = fixture_case("resource-cycles")?;
    let baseline_fds = fd_count()?;
    for cycle in 0..100 {
        let report = authority(&case)?.run(&spec(&["zero"], Duration::from_secs(2))?)?;
        assert!(report.is_success(), "cycle {cycle}: {report:#?}");
        assert_group_absent(report.process.pgid)?;
    }
    assert!(!authority(&case)?.has_unresolved_processes());
    assert_eq!(fd_count()?, baseline_fds);
    Ok(())
}
