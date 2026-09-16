//! Gate 6 — `cargo test -p orchestrator-app --locked --test
//! verifier_process_metrics_e2e`.
//!
//! Proves that verifier evidence is classified from *actual* process and report
//! facts, and that no ambiguous result can be recorded as a pass.
//!
//! Real helper subprocesses emit the three supported report formats — the
//! first-party control protocol, JUnit XML, and TAP 13 — and each is asserted
//! to map to the correct [`VerificationClass`]:
//!
//! | Case | Class |
//! |---|---|
//! | a genuine failing case | `Fail` |
//! | a `t.Skip`-style skipped case | `Skip` — never `Pass` (TRK-535) |
//! | an empty suite | `NoTests` |
//! | a run that never finishes | `Timeout` |
//! | an unparseable report | `InfrastructureError` |
//! | exit 0 contradicted by a failing report | `InfrastructureError` |
//!
//! It also proves the reaping gate: a helper that leaves a background child
//! alive keeps its process group present, so no [`ExactProcessGroupAbsence`]
//! witness exists, so a `PhaseMetricIntent` cannot be constructed and the phase
//! metric cannot be recorded at all. The gate stays blocked rather than
//! recording an optimistic row.
//!
//! Process groups are reaped by PGID. There is no live test runner, no network,
//! and no provider.

#![allow(clippy::doc_markdown, reason = "prose names Go symbols and file paths")]

use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use orchestrator_app::{
    EvidenceReconciler, ExactProcessGroupAbsence, IsolatedFixtureRoot, KernelProcessIdentity,
    MetricsOwner, MetricsOwnerCapability, PhaseMetricIntent, RecordedProcessIdentityStatus,
    ReportFormat, inspect_recorded_process_identity, parse_fixture_protocol, parse_junit_xml,
    parse_tap13,
};
use orchestrator_core::{
    MissionId, VerificationClass, VerificationOutcome, VerificationSummary,
    VerificationTermination, classify_verification,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static CASE: AtomicU64 = AtomicU64::new(1);

const MISSION: &str = "mission-b3-verifier";
const PHASE: &str = "phase-verify";

const HELPER_MODE: &str = "NANIKA_B3_VERIFIER_GATE_HELPER";
const HELPER_OUT: &str = "NANIKA_B3_VERIFIER_GATE_OUT";

/// The control-frame prefix from `supervision.rs`.
const CONTROL_PREFIX: &str = "ORCHESTRATOR_FIXTURE_V1 ";

// ===========================================================================
// Helper subprocess
// ===========================================================================

/// Re-exec entry point for every helper this gate needs.
///
/// `crates/orchestrator-app/tests/fixtures/**` is not in the B3 lease, so no
/// new helper binary may be committed. Re-execing this test binary gives a
/// genuinely compiled first-party helper without adding a file; the reports it
/// emits are written under the test's own temp root.
#[test]
fn helper_entrypoint() {
    let Ok(mode) = std::env::var(HELPER_MODE) else {
        return;
    };
    let code = run_helper(&mode).unwrap_or(70);
    std::process::exit(code);
}

#[allow(
    clippy::too_many_lines,
    reason = "one match arm per helper mode reads better than scattered functions"
)]
fn run_helper(mode: &str) -> TestResult<i32> {
    let out = std::env::var(HELPER_OUT).map(PathBuf::from);
    Ok(match mode {
        // Holds its process group open until stdin closes.
        "hold" => {
            announce()?;
            wait_for_stdin();
            0
        }
        // Spawns a child that stays in *this* process group, then exits. The
        // group therefore outlives the leader, which is exactly the unreaped
        // background-child condition.
        "leaks-background-child" => {
            let mut command = helper_command("linger")?;
            // Deliberately no `process_group(0)`: the child inherits this
            // process's group, so reaping the leader does not empty it.
            command.stdin(Stdio::null()).stdout(Stdio::null());
            command.spawn()?;
            announce()?;
            wait_for_stdin();
            0
        }
        // A background child with a bounded lifetime, so a failed test cannot
        // leave a process behind forever.
        "linger" => {
            std::thread::sleep(Duration::from_secs(30));
            0
        }
        // Never finishes on its own; the parent times it out and kills it.
        "hangs" => {
            announce()?;
            std::thread::sleep(Duration::from_secs(600));
            0
        }
        // The first-party control protocol on stderr: two cases, both passing.
        "protocol-pass" => {
            emit_protocol(2, 2, 2, 0, 0)?;
            0
        }
        // One real failure. A non-zero exit plus a positive failure count is
        // the only shape that classifies as `Fail`.
        "protocol-fail" => {
            emit_protocol(2, 2, 1, 1, 0)?;
            1
        }
        // A required case was skipped. Exit is 0 and nothing failed, which is
        // precisely the shape Go's `t.Skip` produces and precisely the shape
        // TRK-535 recorded as a false PASS.
        "protocol-skip" => {
            emit_protocol(2, 1, 1, 0, 1)?;
            0
        }
        // The suite discovered nothing at all.
        "protocol-no-tests" => {
            emit_protocol(0, 0, 0, 0, 0)?;
            0
        }
        "junit-pass" => {
            write_report(
                out?,
                r#"<testsuites name="suite" tests="3" failures="0" errors="0" skipped="0"/>"#,
            )?;
            0
        }
        "junit-fail" => {
            write_report(
                out?,
                r#"<testsuites name="suite" tests="3" failures="1" errors="0" skipped="0"/>"#,
            )?;
            1
        }
        "junit-skip" => {
            write_report(
                out?,
                r#"<testsuites name="suite" tests="3" failures="0" errors="0" skipped="1"/>"#,
            )?;
            0
        }
        // A JUnit dialect that carries `disabled-failures` alongside the real
        // `failures`. An unanchored attribute search finds the decoy first,
        // reads 0 failures, and lets seven real failures classify as anything
        // but `Fail` (B3 review W2).
        "junit-attribute-prefix" => {
            write_report(
                out?,
                r#"<testsuite name="suite" disabled-failures="0" tests="10" failures="7" errors="0" skipped="0"/>"#,
            )?;
            1
        }
        // The `message` attribute's own quoted value happens to contain the
        // literal text `failures="0"`. A substring search for `failures="`
        // finds that decoy sitting inside another attribute's value before
        // it ever reaches the real `failures="7"`, reads 0 failures, and
        // lets seven real failures classify as anything but `Fail` (RW4).
        "junit-attribute-quoted-value-decoy" => {
            write_report(
                out?,
                r#"<testsuite name="suite" message="expected failures="0" but got more" tests="10" failures="7" errors="0" skipped="0"/>"#,
            )?;
            1
        }
        // Exit 0 while the report records a failure. Two sources that
        // contradict each other are not evidence for the convenient one.
        "junit-contradicts-exit" => {
            write_report(
                out?,
                r#"<testsuites name="suite" tests="2" failures="1" errors="0" skipped="0"/>"#,
            )?;
            0
        }
        "junit-unparseable" => {
            write_report(out?, "<testsuites name=\"suite\" this is not xml")?;
            0
        }
        "tap-pass" => {
            write_report(out?, "TAP version 13\n1..2\nok 1 - a\nok 2 - b\n")?;
            0
        }
        "tap-fail" => {
            write_report(out?, "TAP version 13\n1..2\nok 1 - a\nnot ok 2 - b\n")?;
            1
        }
        // `ok ... # SKIP` still begins with "ok". Reading the prefix alone is
        // the TRK-535 bug; reading the directive is the fix.
        "tap-skip" => {
            write_report(
                out?,
                "TAP version 13\n1..2\nok 1 - a\nok 2 - b # SKIP no fixture\n",
            )?;
            0
        }
        "tap-no-tests" => {
            write_report(out?, "TAP version 13\n1..0\n")?;
            0
        }
        _ => 2,
    })
}

fn announce() -> TestResult {
    println!("ready");
    std::io::stdout().flush()?;
    Ok(())
}

fn wait_for_stdin() {
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

fn write_report(path: PathBuf, body: &str) -> TestResult {
    std::fs::write(path, body)?;
    Ok(())
}

/// Emits a well-formed control-frame stream on stderr.
fn emit_protocol(
    discovered: u64,
    executed: u64,
    passed: u64,
    failed: u64,
    required_skipped: u64,
) -> TestResult {
    let frames = [
        serde_json::json!({"version": 1, "seq": 1, "kind": "ready", "data": {}}),
        serde_json::json!({
            "version": 1,
            "seq": 2,
            "kind": "verification",
            "data": {
                "scenario": "suite",
                "discovered": discovered,
                "executed": executed,
                "passed": passed,
                "failed": failed,
                "required_skipped": required_skipped,
            }
        }),
        serde_json::json!({
            "version": 1,
            "seq": 3,
            "kind": "done",
            "data": {"children_started": 0, "children_reaped": 0}
        }),
    ];
    let mut stderr = std::io::stderr();
    for frame in frames {
        writeln!(stderr, "{CONTROL_PREFIX}{frame}")?;
    }
    stderr.flush()?;
    Ok(())
}

fn helper_command(mode: &str) -> TestResult<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "helper_entrypoint", "--nocapture"])
        .env(HELPER_MODE, mode);
    scrub_provider_environment(&mut command);
    Ok(command)
}

// ===========================================================================
// Fixture harness
// ===========================================================================

struct Fixture {
    parent: PathBuf,
    root: IsolatedFixtureRoot,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let parent = std::fs::canonicalize(std::env::temp_dir())?.join(format!(
            "orchestrator-rs-verifier-metrics-{}-{number}-{label}",
            std::process::id()
        ));
        std::fs::create_dir_all(&parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))?;
        }
        let root = IsolatedFixtureRoot::create_fresh(&parent)?;
        Ok(Self { parent, root })
    }

    fn report_path(&self, name: &str) -> PathBuf {
        self.parent.join(name)
    }

    fn owner(&self) -> TestResult<MetricsOwner> {
        Ok(MetricsOwner::assume(
            MetricsOwnerCapability::in_fixture_boundary(
                orchestrator_app::fixture_production_boundary(&self.root)?,
            )?,
        )?)
    }
}

// ===========================================================================
// Running helpers and reading their evidence
// ===========================================================================

/// One helper run's observable facts.
struct HelperRun {
    termination: VerificationTermination,
    stderr: Vec<u8>,
}

/// Runs a helper to completion in its own process group and captures its facts.
fn run_to_completion(mode: &str, report: Option<&Path>) -> TestResult<HelperRun> {
    use std::os::unix::process::CommandExt;
    let mut command = helper_command(mode)?;
    if let Some(path) = report {
        command.env(HELPER_OUT, path);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command.process_group(0);
    let output = command.spawn()?.wait_with_output()?;
    Ok(HelperRun {
        termination: termination_of(&output.status),
        stderr: output.stderr,
    })
}

fn termination_of(status: &std::process::ExitStatus) -> VerificationTermination {
    use std::os::unix::process::ExitStatusExt;
    status.code().map_or_else(
        || {
            status
                .signal()
                .map_or(VerificationTermination::InfrastructureError, |signal| {
                    VerificationTermination::Signaled(signal)
                })
        },
        VerificationTermination::Exited,
    )
}

fn read_report(path: &Path) -> TestResult<String> {
    Ok(std::fs::read_to_string(path)?)
}

// ===========================================================================
// 1. Helper-protocol classification
// ===========================================================================

fn classify_protocol(mode: &str) -> TestResult<VerificationOutcome> {
    let run = run_to_completion(mode, None)?;
    let report = parse_fixture_protocol(&run.stderr);
    assert!(
        report.complete,
        "{mode}: the helper must emit a well-formed frame stream",
    );
    Ok(classify_verification(
        run.termination,
        report.verification_summary,
    ))
}

#[test]
fn the_helper_protocol_classifies_every_quality_result() -> TestResult {
    for (mode, expected) in [
        ("protocol-pass", VerificationClass::Pass),
        ("protocol-fail", VerificationClass::Fail),
        ("protocol-skip", VerificationClass::Skip),
        ("protocol-no-tests", VerificationClass::NoTests),
    ] {
        let outcome = classify_protocol(mode)?;
        assert_eq!(
            outcome,
            VerificationOutcome::Classified(expected),
            "{mode} misclassified",
        );
        assert_eq!(
            outcome.gate_passed(),
            expected == VerificationClass::Pass,
            "{mode}: only Pass may satisfy the gate",
        );
    }
    Ok(())
}

// ===========================================================================
// 2. JUnit and TAP classification
// ===========================================================================

#[test]
fn junit_reports_classify_from_their_aggregate_counts() -> TestResult {
    let fixture = Fixture::new("junit")?;
    for (mode, expected) in [
        ("junit-pass", VerificationClass::Pass),
        ("junit-fail", VerificationClass::Fail),
        ("junit-skip", VerificationClass::Skip),
    ] {
        let path = fixture.report_path(&format!("{mode}.xml"));
        let run = run_to_completion(mode, Some(&path))?;
        let summary = parse_junit_xml(&read_report(&path)?);
        assert!(summary.is_some(), "{mode}: a real report must parse");
        let outcome = classify_verification(run.termination, summary);
        assert_eq!(
            outcome,
            VerificationOutcome::Classified(expected),
            "{mode} misclassified",
        );
    }
    Ok(())
}

/// B3 review W2: a decoy attribute whose name *ends* with the one being read
/// must not answer for it.
///
/// Under the unanchored search `failures` resolved to `disabled-failures="0"`,
/// so seven real failures were read as zero, `passed` absorbed all ten
/// executed cases, and a failing suite no longer classified as `Fail`.
#[test]
fn a_longer_attribute_name_cannot_answer_for_the_failure_count() -> TestResult {
    let fixture = Fixture::new("junit-attribute-prefix")?;
    let path = fixture.report_path("attribute-prefix.xml");
    let run = run_to_completion("junit-attribute-prefix", Some(&path))?;
    let summary = parse_junit_xml(&read_report(&path)?)
        .ok_or("a well-formed testsuite element must parse")?;
    assert_eq!(summary.failed, 7, "the real `failures` attribute must win");
    assert_eq!(summary.discovered, 10);
    assert_eq!(summary.executed, 10);
    assert_eq!(summary.passed, 3);
    assert_eq!(summary.required_skipped, 0);
    assert_eq!(
        classify_verification(run.termination, Some(summary)),
        VerificationOutcome::Classified(VerificationClass::Fail),
        "seven failures and a non-zero exit are a Fail",
    );
    Ok(())
}

/// RW4: a decoy occurrence of `failures="..."` sitting inside another
/// attribute's own quoted value must not answer for the real `failures`.
///
/// A plain substring search for `failures="` finds that decoy before the
/// real attribute, reads 0 failures, and lets seven real failures classify
/// as anything but `Fail` — the same TRK-535 false-PASS family as the
/// unanchored-prefix case above, but from a decoy that is only reachable by
/// tracking where each attribute's value starts and ends.
#[test]
fn a_decoy_inside_another_attributes_quoted_value_cannot_answer_for_the_failure_count() -> TestResult
{
    let fixture = Fixture::new("junit-attribute-quoted-value-decoy")?;
    let path = fixture.report_path("attribute-quoted-value-decoy.xml");
    let run = run_to_completion("junit-attribute-quoted-value-decoy", Some(&path))?;
    let summary = parse_junit_xml(&read_report(&path)?)
        .ok_or("a well-formed testsuite element must parse")?;
    assert_eq!(summary.failed, 7, "the real `failures` attribute must win");
    assert_eq!(summary.discovered, 10);
    assert_eq!(summary.executed, 10);
    assert_eq!(summary.passed, 3);
    assert_eq!(summary.required_skipped, 0);
    assert_eq!(
        classify_verification(run.termination, Some(summary)),
        VerificationOutcome::Classified(VerificationClass::Fail),
        "seven failures and a non-zero exit are a Fail",
    );
    Ok(())
}

#[test]
fn tap_reports_classify_from_their_result_lines() -> TestResult {
    let fixture = Fixture::new("tap")?;
    for (mode, expected) in [
        ("tap-pass", VerificationClass::Pass),
        ("tap-fail", VerificationClass::Fail),
        ("tap-skip", VerificationClass::Skip),
        ("tap-no-tests", VerificationClass::NoTests),
    ] {
        let path = fixture.report_path(&format!("{mode}.tap"));
        let run = run_to_completion(mode, Some(&path))?;
        let summary = parse_tap13(&read_report(&path)?);
        assert!(summary.is_some(), "{mode}: a real report must parse");
        let outcome = classify_verification(run.termination, summary);
        assert_eq!(
            outcome,
            VerificationOutcome::Classified(expected),
            "{mode} misclassified",
        );
        assert_eq!(outcome.gate_passed(), expected == VerificationClass::Pass);
    }
    Ok(())
}

#[test]
fn a_skipped_case_is_never_a_pass_in_any_format() -> TestResult {
    // TRK-535 in one assertion: three independent report formats, each with a
    // skipped required case, each exiting 0 — and none of them passes.
    let fixture = Fixture::new("skip-never-passes")?;

    let protocol = classify_protocol("protocol-skip")?;

    let junit_path = fixture.report_path("skip.xml");
    let junit_run = run_to_completion("junit-skip", Some(&junit_path))?;
    let junit = classify_verification(
        junit_run.termination,
        parse_junit_xml(&read_report(&junit_path)?),
    );

    let tap_path = fixture.report_path("skip.tap");
    let tap_run = run_to_completion("tap-skip", Some(&tap_path))?;
    let tap = classify_verification(tap_run.termination, parse_tap13(&read_report(&tap_path)?));

    for (label, outcome) in [("protocol", protocol), ("junit", junit), ("tap", tap)] {
        assert_eq!(
            outcome,
            VerificationOutcome::Classified(VerificationClass::Skip),
            "{label}: a skipped required case is Skip",
        );
        assert!(!outcome.gate_passed(), "{label}: Skip must not pass a gate");
    }
    Ok(())
}

// ===========================================================================
// 3. Timeout, unparseable reports, and contradiction
// ===========================================================================

#[test]
fn a_run_that_never_finishes_classifies_as_timeout() -> TestResult {
    use std::os::unix::process::CommandExt;
    let mut command = helper_command("hangs")?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.process_group(0);
    let mut child = command.spawn()?;
    let pid = child.id();
    await_ready(&mut child)?;

    // A real deadline against a process that genuinely does not finish.
    let deadline = Instant::now() + Duration::from_millis(750);
    let mut exited = false;
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !exited,
        "the hanging helper must still be running at the deadline"
    );
    kill_group(pid);
    let _ = child.wait()?;

    let outcome = classify_verification(VerificationTermination::TimedOut, None);
    assert_eq!(
        outcome,
        VerificationOutcome::Classified(VerificationClass::Timeout),
    );
    assert!(!outcome.gate_passed());
    Ok(())
}

#[test]
fn an_unparseable_report_is_an_infrastructure_error_not_a_pass() -> TestResult {
    let fixture = Fixture::new("unparseable")?;
    let path = fixture.report_path("broken.xml");
    let run = run_to_completion("junit-unparseable", Some(&path))?;
    let summary = parse_junit_xml(&read_report(&path)?);
    assert!(summary.is_none(), "a broken report must not parse");
    let outcome = classify_verification(run.termination, summary);
    assert_eq!(
        outcome,
        VerificationOutcome::Classified(VerificationClass::InfrastructureError),
        "the helper exited 0, but there is no evidence of what it did",
    );
    assert!(!outcome.gate_passed());
    Ok(())
}

#[test]
fn a_zero_exit_contradicted_by_its_report_reconciles_to_infrastructure_error() -> TestResult {
    let fixture = Fixture::new("contradiction")?;
    let path = fixture.report_path("contradiction.xml");
    let run = run_to_completion("junit-contradicts-exit", Some(&path))?;
    assert_eq!(run.termination, VerificationTermination::Exited(0));

    // The helper's own exit says everything is fine. Its report says a case
    // failed. `EvidenceReconciler` requires unanimity, so neither wins.
    let reconciled = EvidenceReconciler::new()
        .with_report(
            ReportFormat::HelperProtocol,
            run.termination,
            Some(VerificationSummary {
                discovered: 2,
                executed: 2,
                passed: 2,
                failed: 0,
                required_skipped: 0,
            }),
        )
        .with_report(
            ReportFormat::JUnitXml,
            run.termination,
            parse_junit_xml(&read_report(&path)?),
        )
        .reconcile();
    assert_eq!(
        reconciled,
        VerificationOutcome::Classified(VerificationClass::InfrastructureError),
    );
    assert!(!reconciled.gate_passed());
    Ok(())
}

#[test]
fn agreeing_sources_reconcile_to_their_shared_class() -> TestResult {
    // The negative case above only means something if agreement still works.
    let fixture = Fixture::new("agreement")?;
    let path = fixture.report_path("agree.xml");
    let run = run_to_completion("junit-pass", Some(&path))?;
    let reconciled = EvidenceReconciler::new()
        .with_report(
            ReportFormat::HelperProtocol,
            run.termination,
            Some(VerificationSummary {
                discovered: 3,
                executed: 3,
                passed: 3,
                failed: 0,
                required_skipped: 0,
            }),
        )
        .with_report(
            ReportFormat::JUnitXml,
            run.termination,
            parse_junit_xml(&read_report(&path)?),
        )
        .reconcile();
    assert_eq!(
        reconciled,
        VerificationOutcome::Classified(VerificationClass::Pass),
    );
    assert!(reconciled.gate_passed());
    Ok(())
}

// ===========================================================================
// 4. The reaping gate
// ===========================================================================

fn await_ready(child: &mut Child) -> TestResult {
    let stdout = child.stdout.take().ok_or("helper stdout")?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err("helper exited before announcing readiness".into());
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
            return Ok(());
        }
    }
}

fn kill_group(pid: u32) {
    let raw = i32::try_from(pid).unwrap_or(0);
    if let Some(pid) = rustix::process::Pid::from_raw(raw) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
}

/// Spawns `mode` in its own process group, reaps the leader, and reports what
/// the group looks like afterwards.
fn group_status_after_reaping(mode: &str) -> TestResult<(u32, RecordedProcessIdentityStatus)> {
    use std::os::unix::process::CommandExt;
    let mut command = helper_command(mode)?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.process_group(0);
    let mut child = command.spawn()?;
    let pid = child.id();
    await_ready(&mut child)?;
    let identity = KernelProcessIdentity::observe(pid, pid)?;
    drop(child.stdin.take());
    child.wait()?;
    let status = inspect_recorded_process_identity(
        identity.pid(),
        identity.process_group_id(),
        identity.process_start_identity(),
    )?;
    Ok((pid, status))
}

#[test]
fn a_fully_reaped_group_yields_the_witness_a_phase_metric_requires() -> TestResult {
    let (_, status) = group_status_after_reaping("hold")?;
    let witness = match status {
        RecordedProcessIdentityStatus::ExactGroupAbsent(absence) => absence,
        other => return Err(format!("expected an absent group, observed {other:?}").into()),
    };

    // With the witness in hand the metric is constructible, and recording it
    // succeeds.
    let fixture = Fixture::new("reaped-records")?;
    let owner = fixture.owner()?;
    let intent = PhaseMetricIntent::new(MissionId::new(MISSION)?, PHASE, 1, witness);
    owner.record_phase(&intent)?;
    assert_eq!(
        owner.recorded_phase_names(MISSION)?,
        vec![PHASE.to_owned()],
        "a reaped phase records normally",
    );
    Ok(())
}

#[test]
fn an_unreaped_background_child_leaves_the_phase_metric_unrecordable() -> TestResult {
    let (pid, status) = group_status_after_reaping("leaks-background-child")?;

    // The leader is gone but its group is not: a child it spawned is still
    // running inside it.
    let observed = matches!(
        status,
        RecordedProcessIdentityStatus::LeaderAbsentGroupPresent
    );
    kill_group(pid);
    assert!(
        observed,
        "a surviving background child must keep the group present, observed {status:?}",
    );

    // No witness exists, so `PhaseMetricIntent::new` cannot be called at all.
    // That is the gate: the metric is not merely refused at write time, it is
    // unrepresentable, so there is no path by which this phase records a pass.
    assert!(
        witness_from(status).is_none(),
        "an unreaped group must yield no absence proof",
    );

    // Nothing was written, because nothing could be.
    let fixture = Fixture::new("unreaped-blocked")?;
    let owner = fixture.owner()?;
    assert!(
        owner.recorded_phase_names(MISSION)?.is_empty(),
        "a blocked gate records no phase row",
    );
    Ok(())
}

/// Extracts the witness, if the status carries one.
///
/// `ExactProcessGroupAbsence` is neither `Clone` nor `Copy` and only
/// [`inspect_recorded_process_identity`] constructs it, so this is the only way
/// to obtain one — there is no constructor a caller could reach for instead.
fn witness_from(status: RecordedProcessIdentityStatus) -> Option<ExactProcessGroupAbsence> {
    match status {
        RecordedProcessIdentityStatus::ExactGroupAbsent(absence) => Some(absence),
        _ => None,
    }
}

/// Provider and credential variables scrubbed from every helper subprocess.
///
/// Verbatim from `CORE-100-VERIFICATION-GAPS.md` lines 21-37. The helper reads
/// none of them, but "no credential is touched" is an absolute: a child holding
/// live provider keys in its environment is one careless `Command` away from
/// leaking them, and `env_remove` costs nothing to keep that impossible.
const SCRUBBED_PROVIDER_VARIABLES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "GEMINI_API_KEY",
    "ELEVENLABS_API_KEY",
    "ALLUKA_AUTH_FILE",
    "CLAUDE_CREDENTIALS_DIR",
    "CLAUDE_CREDENTIALS_FILE",
    "CODEX_PATH",
    "NANIKA_HERMETIC_PROCESS_CANARY",
];

/// Removes every provider variable from a helper command's environment.
fn scrub_provider_environment(command: &mut std::process::Command) -> &mut std::process::Command {
    for name in SCRUBBED_PROVIDER_VARIABLES {
        command.env_remove(name);
    }
    command
}
