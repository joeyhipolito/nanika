#[cfg(not(unix))]
compile_error!("kill9_restart_matrix requires Unix SIGKILL and process-group semantics");

use orchestrator_app::{
    AppKnowledgeGateway, CheckpointReconciliationDisposition, CompatibilityProjection,
    DeliveryVerdict, EffectEvidenceCode, ExactProcessGroupAbsence, FixtureAdmissionPolicy,
    FreshFixtureAuthority, IsolatedFixtureRoot, JournalIntent, KernelProcessIdentity, MetricsOwner,
    MetricsOwnerCapability, OutboxState, PHASE_METRIC_TYPE, PhaseMetricIntent,
    PhaseStatus as MetricsPhaseStatus, ProcessNotStartedEvidenceReason, ProcessUncertaintyEvidence,
    R0DurableAttemptReport, R0JournalCrashCell, R0JournalRecoveryReport, R0ProcessCrashCell,
    R0ProcessRecoveryDisposition, RecordedPhase, RecordedProcessIdentityStatus, TokenCounts,
    fixture_production_boundary, inspect_recorded_process_identity, open_fixture_runtime_store,
    phase_metric_payload, prepare_r0_journal_crash, prepare_r0_phase_terminal_crash,
    prepare_r0_process_crash, publication_drain, recover_r0_journal_crash,
    recover_r0_phase_start_continuation, recover_r0_phase_terminal_crash, recover_r0_process_crash,
    run_r0_phase_start_crash_until_worker_projection_barrier, run_r0_process_crash_until_barrier,
};
use orchestrator_core::{MissionId, MissionStatus, PhaseId, PhaseStatus, WorkerId};
use orchestrator_knowledge::{
    CanonicalJson, Delegation, ExpectedHead, FieldMask, Governance, KnowledgeCapability,
    KnowledgeError, LifecycleState, Namespace, Operation, PrimitiveKind, Provenance,
    RecordEnvelope, RecordId, RegistryGeneration, RevisionId, SchemaVersion, Sensitivity,
    TypeManifest, TypeName, TypeRegistry, Validity,
};
use rustix::process::{Pid, Signal, kill_process, kill_process_group, test_kill_process_group};
use std::collections::BTreeSet;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const CHILD_ROLE_ENV: &str = "NANIKA_R0_KILL9_CHILD";
const CHILD_CELL_ENV: &str = "NANIKA_R0_KILL9_CELL";
const CHILD_PARENT_ENV: &str = "NANIKA_R0_KILL9_PARENT";
const CHILD_ROOT_ENV: &str = "NANIKA_R0_KILL9_ROOT";
const CELL2_WORKER_PROJECTION_CUT_ROLE: &str = "cell2-worker-projection-cut";
const ACK_PREFIX: &str = "NANIKA_R0_DURABLE";
const JOURNAL_MISSION: &str = "r0-journal-restart";
const JOURNAL_PHASE: &str = "verify";
const PHASE_TERMINAL_MISSION: &str = "r0-phase-terminal-restart";
const PHASE_TERMINAL_PHASE: &str = "verify";
const PHASE_TERMINAL_PERSONA: &str = "r0-provider";
const PHASE_START_PERSONA: &str = "r0-process";
const DURABLE_CANARY_SENTINEL: &str = ".nanika-durable-canary-launched";
const DURABLE_CANARY_DUPLICATE: &str = ".nanika-durable-canary-duplicate";
const DURABLE_CANARY_CONTENT: &[u8] = b"nanika-durable-canary-v1\n";
/// Cell 12: the mission whose phase metric is committed but never acknowledged.
const METRICS_MISSION: &str = "r0-metrics-restart";
const METRICS_PHASE: &str = "verify";
const METRICS_NAMESPACE: &str = "metrics";
const METRICS_AT: &str = "2026-08-26T12:00:00Z";
/// Fixture mode of the reaped witness a phase metric cannot be built without.
const METRICS_WITNESS_MODE: &str = "stdin-race";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CellKind {
    MissionJournalBeforeProjection,
    PhaseJournalBeforeProjection,
    ProcessPendingBeforeClaim,
    ProcessClaimedBeforeSpecification,
    ProcessReleaseAuthorizedBeforeStarted,
    ProcessStartedObservedBeforeTerminal,
    ProcessTerminalObservationBeforeDecision,
    ProcessTerminalDecisionBeforeProjection,
    PhaseTerminalBeforeMissionTerminal,
    EventAppendBeforeCheckpoint,
    CheckpointFsyncBeforeReceipt,
    MetricsCommitBeforeAcknowledgement,
}

impl CellKind {
    const fn name(self) -> &'static str {
        match self {
            Self::MissionJournalBeforeProjection => "mission-journal-before-projection",
            Self::PhaseJournalBeforeProjection => "phase-journal-before-projection",
            Self::ProcessPendingBeforeClaim => "process-pending-before-claim",
            Self::ProcessClaimedBeforeSpecification => "claim-before-spec-init",
            Self::ProcessReleaseAuthorizedBeforeStarted => {
                "release-authorization-before-spawn-observation"
            }
            Self::ProcessStartedObservedBeforeTerminal => {
                "spawned-observation-before-terminal-observation"
            }
            Self::ProcessTerminalObservationBeforeDecision => {
                "terminal-observation-before-terminal-decision"
            }
            Self::ProcessTerminalDecisionBeforeProjection => {
                "terminal-decision-before-worker-checkpoint"
            }
            Self::PhaseTerminalBeforeMissionTerminal => "phase-terminal-before-mission-terminal",
            Self::EventAppendBeforeCheckpoint => "event-append-before-checkpoint",
            Self::CheckpointFsyncBeforeReceipt => "checkpoint-fsync-before-receipt",
            Self::MetricsCommitBeforeAcknowledgement => "metrics-commit-before-acknowledgement",
        }
    }

    const fn process_cell(self) -> Option<R0ProcessCrashCell> {
        match self {
            Self::ProcessPendingBeforeClaim => Some(R0ProcessCrashCell::PendingBeforeClaim),
            Self::ProcessClaimedBeforeSpecification => {
                Some(R0ProcessCrashCell::ClaimedBeforeSpecification)
            }
            Self::ProcessReleaseAuthorizedBeforeStarted => {
                Some(R0ProcessCrashCell::ReleaseAuthorizedBeforeStarted)
            }
            Self::ProcessStartedObservedBeforeTerminal => {
                Some(R0ProcessCrashCell::StartedObservedBeforeTerminal)
            }
            Self::ProcessTerminalObservationBeforeDecision => {
                Some(R0ProcessCrashCell::TerminalObservationBeforeDecision)
            }
            Self::ProcessTerminalDecisionBeforeProjection => {
                Some(R0ProcessCrashCell::TerminalDecisionBeforeProjection)
            }
            Self::MissionJournalBeforeProjection
            | Self::PhaseJournalBeforeProjection
            | Self::PhaseTerminalBeforeMissionTerminal
            | Self::EventAppendBeforeCheckpoint
            | Self::CheckpointFsyncBeforeReceipt
            | Self::MetricsCommitBeforeAcknowledgement => None,
        }
    }

    const fn journal_cell(self) -> Option<R0JournalCrashCell> {
        match self {
            Self::MissionJournalBeforeProjection => {
                Some(R0JournalCrashCell::MissionStartBeforeProjection)
            }
            Self::PhaseJournalBeforeProjection => {
                Some(R0JournalCrashCell::PhaseStartBeforeProjection)
            }
            Self::EventAppendBeforeCheckpoint => {
                Some(R0JournalCrashCell::EventAppendBeforeCheckpoint)
            }
            Self::CheckpointFsyncBeforeReceipt => {
                Some(R0JournalCrashCell::CheckpointFsyncBeforeReceipt)
            }
            Self::ProcessPendingBeforeClaim
            | Self::ProcessClaimedBeforeSpecification
            | Self::ProcessReleaseAuthorizedBeforeStarted
            | Self::ProcessStartedObservedBeforeTerminal
            | Self::ProcessTerminalObservationBeforeDecision
            | Self::ProcessTerminalDecisionBeforeProjection
            | Self::PhaseTerminalBeforeMissionTerminal
            | Self::MetricsCommitBeforeAcknowledgement => None,
        }
    }

    const fn expected_disposition(self) -> Option<CheckpointReconciliationDisposition> {
        match self {
            Self::MissionJournalBeforeProjection
            | Self::PhaseJournalBeforeProjection
            | Self::EventAppendBeforeCheckpoint => {
                Some(CheckpointReconciliationDisposition::Published)
            }
            Self::CheckpointFsyncBeforeReceipt => {
                Some(CheckpointReconciliationDisposition::AlreadyTarget)
            }
            Self::ProcessPendingBeforeClaim
            | Self::ProcessClaimedBeforeSpecification
            | Self::ProcessReleaseAuthorizedBeforeStarted
            | Self::ProcessStartedObservedBeforeTerminal
            | Self::ProcessTerminalObservationBeforeDecision
            | Self::ProcessTerminalDecisionBeforeProjection
            | Self::PhaseTerminalBeforeMissionTerminal
            | Self::MetricsCommitBeforeAcknowledgement => None,
        }
    }

    const fn expected_record_count(self) -> Option<usize> {
        match self {
            Self::MissionJournalBeforeProjection => Some(2),
            Self::PhaseJournalBeforeProjection => Some(3),
            Self::EventAppendBeforeCheckpoint | Self::CheckpointFsyncBeforeReceipt => Some(4),
            Self::ProcessPendingBeforeClaim
            | Self::ProcessClaimedBeforeSpecification
            | Self::ProcessReleaseAuthorizedBeforeStarted
            | Self::ProcessStartedObservedBeforeTerminal
            | Self::ProcessTerminalObservationBeforeDecision
            | Self::ProcessTerminalDecisionBeforeProjection
            | Self::PhaseTerminalBeforeMissionTerminal
            | Self::MetricsCommitBeforeAcknowledgement => None,
        }
    }

    const fn uses_actor_barrier(self) -> bool {
        matches!(
            self,
            Self::ProcessClaimedBeforeSpecification
                | Self::ProcessReleaseAuthorizedBeforeStarted
                | Self::ProcessStartedObservedBeforeTerminal
        )
    }
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

fn remove_private_tree(path: &Path) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return std::fs::remove_file(path);
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    for entry in std::fs::read_dir(path)? {
        remove_private_tree(&entry?.path())?;
    }
    std::fs::remove_dir(path)
}

fn copy_private_executable(source: &Path, target: &Path) -> TestResult {
    let source_metadata = std::fs::metadata(source)?;
    if !source_metadata.is_file() || source_metadata.permissions().mode() & 0o111 == 0 {
        return Err("R0 controller source is not an executable regular file".into());
    }
    std::fs::copy(source, target)?;
    std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o500))?;
    let target_file = std::fs::File::open(target)?;
    let target_metadata = target_file.metadata()?;
    if !target_metadata.is_file()
        || target_metadata.permissions().mode() & 0o7777 != 0o500
        || target_metadata.len() != source_metadata.len()
    {
        return Err("R0 controller executable copy failed verification".into());
    }
    target_file.sync_all()?;
    Ok(())
}

fn materialize_private_controller(parent: &Path) -> TestResult<PathBuf> {
    let controller = parent.join("controller");
    private_dir(&controller)?;
    let runner = controller.join("kill9-restart-matrix");
    copy_private_executable(&std::env::current_exe()?, &runner)?;
    copy_private_executable(
        Path::new(env!("CARGO_BIN_EXE_orchestrator-r0-process-broker-fixture")),
        &controller.join("orchestrator-process-broker"),
    )?;
    std::fs::File::open(&controller)?.sync_all()?;
    Ok(runner)
}

fn checked_pid(raw: u32) -> Option<Pid> {
    i32::try_from(raw).ok().and_then(Pid::from_raw)
}

fn owned_process_fixture_bytes() -> TestResult<Vec<u8>> {
    Ok(std::fs::read(env!(
        "CARGO_BIN_EXE_orchestrator-owned-process-fixture"
    ))?)
}

fn policy(parent: &Path) -> TestResult<FixtureAdmissionPolicy> {
    let helper = owned_process_fixture_bytes()?;
    Ok(FixtureAdmissionPolicy::new(
        parent.join("synthetic-user-home"),
        std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?,
        std::fs::canonicalize(std::env::temp_dir())?,
    )
    .with_expected_fixture_helper(&helper))
}

fn workspace_root(root: &Path) -> PathBuf {
    root.join("workspaces").join(JOURNAL_MISSION)
}

fn event_path(root: &Path) -> PathBuf {
    root.join("events").join(format!("{JOURNAL_MISSION}.jsonl"))
}

fn checkpoint_path(root: &Path) -> PathBuf {
    workspace_root(root).join("checkpoint.json")
}

fn phase_terminal_workspace_root(root: &Path) -> PathBuf {
    root.join("workspaces").join(PHASE_TERMINAL_MISSION)
}

fn phase_terminal_event_path(root: &Path) -> PathBuf {
    phase_terminal_workspace_root(root).join("events.jsonl")
}

fn phase_terminal_checkpoint_path(root: &Path) -> PathBuf {
    phase_terminal_workspace_root(root).join("checkpoint.json")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

fn file_identity(path: &Path) -> TestResult<FileIdentity> {
    let metadata = std::fs::metadata(path)?;
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn raw_event_rows(root: &Path) -> TestResult<Vec<(u64, String)>> {
    raw_event_rows_at(&event_path(root))
}

fn raw_event_rows_at(path: &Path) -> TestResult<Vec<(u64, String)>> {
    std::fs::read(path)?
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let event: serde_json::Value = serde_json::from_slice(line)?;
            let sequence = event
                .get("sequence")
                .and_then(serde_json::Value::as_u64)
                .ok_or("raw event sequence is missing")?;
            let event_type = event
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or("raw event type is missing")?;
            Ok((sequence, event_type))
        })
        .collect()
}

fn raw_checkpoint_statuses(root: &Path) -> TestResult<(String, String)> {
    raw_checkpoint_statuses_at(&checkpoint_path(root))
}

fn raw_checkpoint_statuses_at(path: &Path) -> TestResult<(String, String)> {
    raw_checkpoint_statuses_from_bytes(&std::fs::read(path)?)
}

fn raw_checkpoint_statuses_from_bytes(bytes: &[u8]) -> TestResult<(String, String)> {
    let checkpoint: serde_json::Value = serde_json::from_slice(bytes)?;
    let payload = checkpoint
        .get("payload")
        .ok_or("raw checkpoint payload is missing")?;
    let mission = payload
        .get("status")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or("raw checkpoint mission status is missing")?;
    let phase = payload
        .get("plan")
        .and_then(|plan| plan.get("phases"))
        .and_then(serde_json::Value::as_array)
        .and_then(|phases| phases.first())
        .and_then(|phase| phase.get("status"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or("raw checkpoint phase status is missing")?;
    Ok((mission, phase))
}

fn assert_raw_projection(root: &Path, report: &R0JournalRecoveryReport) -> TestResult {
    let rows = raw_event_rows(root)?;
    assert_eq!(
        rows.iter()
            .map(|(_, event_type)| event_type.as_str())
            .collect::<Vec<_>>(),
        report
            .event_types()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        rows.iter()
            .map(|(sequence, _)| *sequence)
            .collect::<Vec<_>>(),
        (1..=u64::try_from(rows.len())?).collect::<Vec<_>>()
    );
    let (mission, phase) = raw_checkpoint_statuses(root)?;
    assert_eq!(mission, report.mission_status());
    assert_eq!(phase, report.phase_status());
    Ok(())
}

fn write_crash_harness_ack(cell: CellKind, root: &Path) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    // A `--nocapture` libtest child writes `test <name> ... ` without a
    // newline before entering the test. End that harness-owned prefix first
    // so the parent always receives the acknowledgement as one exact frame.
    stdout.write_all(b"\n")?;
    writeln!(stdout, "{ACK_PREFIX} {} {}", cell.name(), root.display())?;
    stdout.flush()
}

fn durable_ack(cell: CellKind, root: &Path) -> TestResult {
    write_crash_harness_ack(cell, root)?;

    let mut stdin = std::io::stdin().lock();
    let mut line = String::new();
    let _closed_without_sigkill = stdin.read_line(&mut line)?;
    Err("R0 child controller closed without SIGKILL".into())
}

fn process_helper_marker_paths(root: &Path) -> TestResult<(PathBuf, PathBuf)> {
    let phase = PhaseId::new(PHASE_TERMINAL_PHASE)?;
    let worker = WorkerId::for_phase(PHASE_TERMINAL_PERSONA, &phase)?;
    let worker_root = phase_terminal_workspace_root(root)
        .join("workers")
        .join(worker.as_str());
    Ok((
        worker_root.join(DURABLE_CANARY_SENTINEL),
        worker_root.join(DURABLE_CANARY_DUPLICATE),
    ))
}

fn phase_start_helper_marker_paths(root: &Path) -> TestResult<(PathBuf, PathBuf)> {
    let phase = PhaseId::new(JOURNAL_PHASE)?;
    let worker = WorkerId::for_phase(PHASE_START_PERSONA, &phase)?;
    let worker_root = workspace_root(root).join("workers").join(worker.as_str());
    Ok((
        worker_root.join(DURABLE_CANARY_SENTINEL),
        worker_root.join(DURABLE_CANARY_DUPLICATE),
    ))
}

fn require_path_absent(path: &Path) -> TestResult {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(format!("unexpected helper marker at {}", path.display()).into()),
        Err(error) => Err(error.into()),
    }
}

fn durable_actor_barrier_ack_and_park(cell: CellKind, root: &Path) -> ! {
    let result = (|| -> TestResult {
        if cell == CellKind::ProcessReleaseAuthorizedBeforeStarted {
            let (sentinel, duplicate) = process_helper_marker_paths(root)?;
            require_path_absent(&sentinel)?;
            require_path_absent(&duplicate)?;
        }
        write_crash_harness_ack(cell, root)?;
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!(
            "{} barrier does not match the two-phase launch protocol: {error}",
            cell.name()
        );
        std::process::exit(70);
    }
    loop {
        std::thread::park();
    }
}

fn phase_start_worker_projection_barrier_ack_and_park(
    cell: CellKind,
    root: &Path,
    process: R0DurableAttemptReport,
) -> ! {
    let result = (|| -> TestResult {
        if cell != CellKind::PhaseJournalBeforeProjection {
            return Err("Cell 2 worker-projection barrier received the wrong cell".into());
        }
        if process.state() != OutboxState::Succeeded
            || process.attempts() != 1
            || process.observation_state() != OutboxState::Succeeded
            || process.evidence_code() != EffectEvidenceCode::ExitObservedSuccess
            || !process.execution_identity_present()
            || process.observation_history_count() != 1
            || !process.terminal_decision_present()
            || !process.helper_executed()
        {
            return Err(format!(
                "Cell 2 worker-projection cut lacks exact durable terminal evidence: {process:?}"
            )
            .into());
        }
        if raw_event_rows(root)?
            != [
                (1, "mission.started".to_owned()),
                (2, "phase.started".to_owned()),
            ]
        {
            return Err(
                "Cell 2 worker events were published before the worker-projection barrier".into(),
            );
        }
        if raw_checkpoint_statuses(root)? != ("in_progress".to_owned(), "running".to_owned()) {
            return Err(
                "Cell 2 checkpoint left the exact running phase before worker projection".into(),
            );
        }
        let (sentinel, duplicate) = phase_start_helper_marker_paths(root)?;
        if std::fs::read(&sentinel)? != DURABLE_CANARY_CONTENT {
            return Err("Cell 2 helper sentinel content is not exact at the cut".into());
        }
        require_path_absent(&duplicate)?;

        write_crash_harness_ack(cell, root)?;
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!(
            "{} worker-projection barrier is not exact: {error}",
            cell.name()
        );
        std::process::exit(70);
    }
    loop {
        std::thread::park();
    }
}

fn child_main(cell: CellKind, parent: &Path) -> TestResult {
    if cell == CellKind::MetricsCommitBeforeAcknowledgement {
        return metrics_commit_child_main(cell, parent);
    }
    let isolated = IsolatedFixtureRoot::create_fresh(parent)?;
    let root = isolated.path().to_path_buf();
    let authority = FreshFixtureAuthority::admit(isolated, &policy(parent)?)?;
    if cell.uses_actor_barrier() {
        let helper = owned_process_fixture_bytes()?;
        let process_cell = cell
            .process_cell()
            .ok_or("actor barrier cell has no process mapping")?;
        let ack_root = root.clone();
        run_r0_process_crash_until_barrier(authority, &helper, process_cell, move || {
            durable_actor_barrier_ack_and_park(cell, &ack_root);
        })?;
        return Err("R0 actor barrier returned without SIGKILL".into());
    }
    if let Some(process_cell) = cell.process_cell() {
        let helper = owned_process_fixture_bytes()?;
        let _guard = prepare_r0_process_crash(authority, &helper, process_cell)?;
        return durable_ack(cell, &root);
    }
    if let Some(journal_cell) = cell.journal_cell() {
        let _guard = prepare_r0_journal_crash(authority, journal_cell)?;
        return durable_ack(cell, &root);
    }
    let helper = owned_process_fixture_bytes()?;
    let _guard = prepare_r0_phase_terminal_crash(authority, &helper)?;
    durable_ack(cell, &root)
}

fn phase_start_worker_projection_cut_main(root: &Path, parent: &Path) -> TestResult {
    let authority = recover_authority(root, parent)?;
    let helper = owned_process_fixture_bytes()?;
    let root = root.to_path_buf();
    run_r0_phase_start_crash_until_worker_projection_barrier(authority, &helper, move |process| {
        phase_start_worker_projection_barrier_ack_and_park(
            CellKind::PhaseJournalBeforeProjection,
            &root,
            process,
        );
    })?;
    Err("Cell 2 worker-projection barrier returned without SIGKILL".into())
}

fn recover_authority(root: &Path, parent: &Path) -> TestResult<FreshFixtureAuthority> {
    Ok(FreshFixtureAuthority::recover(
        IsolatedFixtureRoot::identify(root)?,
        &policy(parent)?,
    )?)
}

fn assert_report(cell: CellKind, report: &R0JournalRecoveryReport) -> TestResult {
    match cell {
        CellKind::MissionJournalBeforeProjection => {
            assert_eq!(report.event_types(), ["mission.started"]);
            assert_eq!(report.mission_status(), "in_progress");
            assert_eq!(report.phase_status(), "pending");
        }
        CellKind::PhaseJournalBeforeProjection => {
            assert_eq!(report.event_types(), ["mission.started", "phase.started"]);
            assert_eq!(report.mission_status(), "in_progress");
            assert_eq!(report.phase_status(), "running");
        }
        CellKind::EventAppendBeforeCheckpoint | CellKind::CheckpointFsyncBeforeReceipt => {
            assert_eq!(
                report.event_types(),
                ["mission.started", "phase.started", "phase.completed"]
            );
            assert_eq!(report.mission_status(), "in_progress");
            assert_eq!(report.phase_status(), "completed");
        }
        CellKind::PhaseTerminalBeforeMissionTerminal => {
            return Err("cell 9 reached compatibility projector assertions".into());
        }
        CellKind::ProcessPendingBeforeClaim
        | CellKind::ProcessClaimedBeforeSpecification
        | CellKind::ProcessReleaseAuthorizedBeforeStarted
        | CellKind::ProcessStartedObservedBeforeTerminal
        | CellKind::ProcessTerminalObservationBeforeDecision
        | CellKind::ProcessTerminalDecisionBeforeProjection => {
            return Err("process crash cell reached compatibility projector assertions".into());
        }
        CellKind::MetricsCommitBeforeAcknowledgement => {
            return Err("cell 12 reached compatibility projector assertions".into());
        }
    }
    let expected_records = cell
        .expected_record_count()
        .ok_or("journal cell has no compatibility record count")?;
    assert_eq!(report.journal_record_count(), expected_records);
    assert_eq!(report.acknowledged_record_count(), expected_records);
    assert_eq!(
        report.projection_receipt_count(),
        expected_records.saturating_mul(2)
    );
    assert_eq!(report.incomplete_record_count(), 0);
    assert_eq!(report.target_transition_count(), 1);
    Ok(())
}

fn restart_phase_terminal_and_assert(root: &Path, parent: &Path) -> TestResult {
    let event_path = phase_terminal_event_path(root);
    let checkpoint_path = phase_terminal_checkpoint_path(root);
    let event_bytes_at_cut = std::fs::read(&event_path)?;
    let checkpoint_bytes_at_cut = std::fs::read(&checkpoint_path)?;
    let rows_at_cut = raw_event_rows_at(&event_path)?;
    assert_eq!(
        rows_at_cut,
        [
            (1, "mission.started".to_owned()),
            (2, "phase.started".to_owned()),
            (3, "worker.spawned".to_owned()),
            (4, "worker.completed".to_owned()),
            (5, "phase.completed".to_owned()),
        ]
    );
    assert_eq!(
        raw_checkpoint_statuses_at(&checkpoint_path)?,
        ("in_progress".to_owned(), "completed".to_owned())
    );

    let helper = owned_process_fixture_bytes()?;
    let first = recover_r0_phase_terminal_crash(recover_authority(root, parent)?, &helper)?;
    assert_eq!(first.mission_status(), MissionStatus::Completed);
    assert_eq!(first.phase_status(), PhaseStatus::Completed);
    assert_eq!(
        first.event_types(),
        [
            "mission.started",
            "phase.started",
            "worker.spawned",
            "worker.completed",
            "phase.completed",
            "mission.completed",
        ]
    );
    let process = first.process();
    assert_eq!(process.state(), OutboxState::Succeeded);
    assert_eq!(process.attempts(), 1);
    assert_eq!(process.observation_state(), OutboxState::Succeeded);
    assert_eq!(
        process.evidence_code(),
        EffectEvidenceCode::ExitObservedSuccess
    );
    assert!(process.execution_identity_present());
    assert_eq!(process.observation_history_count(), 1);
    assert_eq!(std::fs::read(&event_path)?, first.event_bytes());
    assert_eq!(std::fs::read(&checkpoint_path)?, first.checkpoint_bytes());
    assert!(
        first.event_bytes().starts_with(&event_bytes_at_cut),
        "cell 9 rewrote an already-published event prefix"
    );
    let appended = &first.event_bytes()[event_bytes_at_cut.len()..];
    let appended_rows = appended
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let [mission_terminal] = appended_rows.as_slice() else {
        return Err("cell 9 did not append exactly one mission terminal".into());
    };
    let mission_terminal: serde_json::Value = serde_json::from_slice(mission_terminal)?;
    assert_eq!(
        mission_terminal
            .get("sequence")
            .and_then(serde_json::Value::as_u64),
        Some(6)
    );
    assert_eq!(
        mission_terminal
            .get("type")
            .and_then(serde_json::Value::as_str),
        Some("mission.completed")
    );
    assert_ne!(checkpoint_bytes_at_cut, first.checkpoint_bytes());
    assert_eq!(
        raw_checkpoint_statuses_at(&checkpoint_path)?,
        ("completed".to_owned(), "completed".to_owned())
    );

    let second = recover_r0_phase_terminal_crash(recover_authority(root, parent)?, &helper)?;
    assert_eq!(second, first, "cell 9 stable reopen changed durable state");
    assert_eq!(std::fs::read(&event_path)?, first.event_bytes());
    assert_eq!(std::fs::read(&checkpoint_path)?, first.checkpoint_bytes());
    Ok(())
}

fn restart_phase_start_and_assert(root: &Path, parent: &Path) -> TestResult {
    let event_bytes_at_cut = std::fs::read(event_path(root))?;
    let checkpoint_bytes_at_cut = std::fs::read(checkpoint_path(root))?;
    let checkpoint_identity_at_cut = file_identity(&checkpoint_path(root))?;
    let (sentinel, duplicate) = phase_start_helper_marker_paths(root)?;
    assert_eq!(std::fs::read(&sentinel)?, DURABLE_CANARY_CONTENT);
    require_path_absent(&duplicate)?;
    let sentinel_identity_at_cut = file_identity(&sentinel)?;

    let helper = owned_process_fixture_bytes()?;
    let first = recover_r0_phase_start_continuation(recover_authority(root, parent)?, &helper)?;
    assert_eq!(
        first.event_types(),
        [
            "mission.started",
            "phase.started",
            "worker.spawned",
            "worker.completed",
        ]
    );
    assert_eq!(
        raw_event_rows(root)?,
        [
            (1, "mission.started".to_owned()),
            (2, "phase.started".to_owned()),
            (3, "worker.spawned".to_owned()),
            (4, "worker.completed".to_owned()),
        ]
    );
    assert_eq!(first.mission_status(), "in_progress");
    assert_eq!(first.phase_status(), "running");
    assert_eq!(first.journal_record_count(), 5);
    assert_eq!(first.acknowledged_record_count(), 5);
    assert_eq!(first.projection_receipt_count(), 10);
    assert_eq!(first.incomplete_record_count(), 0);
    assert_eq!(first.phase_start_transition_count(), 1);
    assert_eq!(first.recovery_checkpoint_disposition(), None);
    let expected_worker = WorkerId::for_phase(PHASE_START_PERSONA, &PhaseId::new(JOURNAL_PHASE)?)?;
    assert_eq!(first.worker_id(), expected_worker.as_str());
    assert_eq!(first.attempt(), 1);
    assert_eq!(first.terminal_event_type(), "worker.completed");
    assert_eq!(first.spawned_at_utc(), first.durable_committed_at_utc());
    assert_eq!(first.terminal_at_utc(), first.durable_committed_at_utc());
    assert!(!first.projection_replayed());
    let process = first.durable_attempt();
    assert_eq!(process.state(), OutboxState::Succeeded);
    assert_eq!(process.attempts(), 1);
    assert_eq!(process.observation_state(), OutboxState::Succeeded);
    assert_eq!(
        process.evidence_code(),
        EffectEvidenceCode::ExitObservedSuccess
    );
    assert!(process.execution_identity_present());
    assert_eq!(process.observation_history_count(), 1);
    assert!(process.terminal_decision_present());
    assert!(process.helper_executed());
    assert_eq!(std::fs::read(&sentinel)?, DURABLE_CANARY_CONTENT);
    require_path_absent(&duplicate)?;
    assert_eq!(
        file_identity(&sentinel)?,
        sentinel_identity_at_cut,
        "Cell 2 first recovery replaced the exact helper sentinel"
    );
    assert_eq!(std::fs::read(event_path(root))?, first.event_bytes());
    assert_eq!(
        std::fs::read(checkpoint_path(root))?,
        first.checkpoint_bytes()
    );
    assert!(first.event_bytes().starts_with(&event_bytes_at_cut));
    assert_eq!(checkpoint_bytes_at_cut, first.checkpoint_bytes());
    assert_eq!(
        file_identity(&checkpoint_path(root))?,
        checkpoint_identity_at_cut,
        "Cell 2 worker projection replaced an already-target checkpoint"
    );
    assert_eq!(
        raw_checkpoint_statuses(root)?,
        ("in_progress".to_owned(), "running".to_owned())
    );

    let second = recover_r0_phase_start_continuation(recover_authority(root, parent)?, &helper)?;
    assert!(second.projection_replayed());
    assert_eq!(second.recovery_checkpoint_disposition(), None);
    assert_eq!(second.event_types(), first.event_types());
    assert_eq!(second.event_bytes(), first.event_bytes());
    assert_eq!(second.checkpoint_bytes(), first.checkpoint_bytes());
    assert_eq!(second.worker_id(), first.worker_id());
    assert_eq!(second.attempt(), first.attempt());
    assert_eq!(second.terminal_event_type(), first.terminal_event_type());
    assert_eq!(second.spawned_at_utc(), first.spawned_at_utc());
    assert_eq!(second.terminal_at_utc(), first.terminal_at_utc());
    assert_eq!(
        second.durable_committed_at_utc(),
        first.durable_committed_at_utc()
    );
    assert_eq!(second.durable_attempt(), first.durable_attempt());
    assert_eq!(second.journal_record_count(), first.journal_record_count());
    assert_eq!(
        second.acknowledged_record_count(),
        first.acknowledged_record_count()
    );
    assert_eq!(
        second.projection_receipt_count(),
        first.projection_receipt_count()
    );
    assert_eq!(
        second.phase_start_transition_count(),
        first.phase_start_transition_count()
    );
    assert_eq!(std::fs::read(&sentinel)?, DURABLE_CANARY_CONTENT);
    assert_eq!(file_identity(&sentinel)?, sentinel_identity_at_cut);
    require_path_absent(&duplicate)?;
    assert_eq!(std::fs::read(event_path(root))?, first.event_bytes());
    assert_eq!(
        std::fs::read(checkpoint_path(root))?,
        first.checkpoint_bytes()
    );
    assert_eq!(
        file_identity(&checkpoint_path(root))?,
        checkpoint_identity_at_cut,
        "Cell 2 stable reopen replaced checkpoint.json"
    );
    Ok(())
}

fn restart_process_and_assert(
    cell: CellKind,
    process_cell: R0ProcessCrashCell,
    root: &Path,
    parent: &Path,
) -> TestResult {
    let helper = owned_process_fixture_bytes()?;
    let report = recover_r0_process_crash(recover_authority(root, parent)?, &helper, process_cell)?;
    assert_eq!(report.cell(), process_cell);
    assert_eq!(
        std::fs::read(phase_terminal_event_path(root))?,
        report.event_bytes()
    );
    assert_eq!(
        std::fs::read(phase_terminal_checkpoint_path(root))?,
        report.checkpoint_bytes()
    );

    match cell {
        CellKind::ProcessClaimedBeforeSpecification => {
            assert_eq!(report.cut_state(), OutboxState::Executing);
            assert_eq!(report.cut_attempts(), 1);
            assert!(!report.cut_execution_identity_present());
            assert_eq!(report.cut_observation_history_count(), 0);
            assert!(!report.cut_terminal_decision_present());
            assert_eq!(report.cut_claim_marker_count(), 1);
            assert_eq!(report.cut_spawn_permit_count(), 0);
            assert_eq!(report.cut_release_authorization_count(), 0);
            assert_eq!(report.cut_started_observed_marker_count(), 0);
            assert_eq!(
                report.cut_event_types(),
                ["mission.started", "phase.started"]
            );
            assert!(!report.cut_helper_executed());
            assert_eq!(
                raw_checkpoint_statuses_from_bytes(report.cut_checkpoint_bytes())?,
                ("in_progress".to_owned(), "running".to_owned())
            );

            assert_eq!(report.recovered_state(), OutboxState::Failed);
            assert_eq!(report.recovered_attempts(), 1);
            assert!(!report.recovered_execution_identity_present());
            assert_eq!(report.recovered_observation_history_count(), 1);
            assert_eq!(
                report.recovered_evidence_code(),
                Some(EffectEvidenceCode::ProcessNotStarted)
            );
            assert_eq!(
                report.recovered_not_started_reason(),
                Some(ProcessNotStartedEvidenceReason::RecoveredBeforeSpawn)
            );
            assert_eq!(report.recovered_uncertainty(), None);
            assert!(report.recovered_terminal_decision_present());
            assert_eq!(report.recovered_claim_marker_count(), 1);
            assert_eq!(report.recovered_spawn_permit_count(), 0);
            assert_eq!(report.recovered_release_authorization_count(), 0);
            assert_eq!(report.recovered_started_observed_marker_count(), 0);
            assert_eq!(
                report.first_recovery_disposition(),
                Some(R0ProcessRecoveryDisposition::RecoveredBeforeSpawnNotStarted)
            );
            assert_eq!(report.second_recovery_disposition(), None);
            assert!(!report.recovered_process_group_absent());
            assert!(!report.helper_executed());
            assert_eq!(report.mission_status(), MissionStatus::Failed);
            assert_eq!(report.phase_status(), PhaseStatus::Failed);
            assert_eq!(
                report.event_types(),
                [
                    "mission.started",
                    "phase.started",
                    "worker.spawned",
                    "worker.failed",
                    "phase.failed",
                    "mission.failed",
                ]
            );
            assert!(
                report.event_bytes().starts_with(report.cut_event_bytes()),
                "cell 4 recovery rewrote the durable event prefix"
            );
            assert_eq!(
                raw_checkpoint_statuses_from_bytes(report.checkpoint_bytes())?,
                ("failed".to_owned(), "failed".to_owned())
            );
        }
        CellKind::ProcessReleaseAuthorizedBeforeStarted
        | CellKind::ProcessStartedObservedBeforeTerminal => {
            let started_observed =
                u32::from(cell == CellKind::ProcessStartedObservedBeforeTerminal);
            assert_eq!(report.cut_state(), OutboxState::Executing);
            assert_eq!(report.cut_attempts(), 1);
            let identity = report
                .cut_execution_identity()
                .ok_or("cell 5/6 cut is missing its exact process identity")?;
            assert_eq!(identity.attempt(), 1);
            assert_ne!(identity.pid(), 0);
            assert_ne!(identity.process_group_id(), 0);
            assert!(!identity.process_start_identity().is_empty());
            assert_eq!(report.cut_observation_history_count(), 0);
            assert!(!report.cut_terminal_decision_present());
            assert_eq!(report.cut_claim_marker_count(), 1);
            assert_eq!(report.cut_spawn_permit_count(), 1);
            assert_eq!(report.cut_release_authorization_count(), 1);
            assert_eq!(report.cut_started_observed_marker_count(), started_observed);
            assert_eq!(
                report.cut_event_types(),
                ["mission.started", "phase.started"]
            );
            assert_eq!(
                raw_checkpoint_statuses_from_bytes(report.cut_checkpoint_bytes())?,
                ("in_progress".to_owned(), "running".to_owned())
            );

            assert_eq!(report.recovered_state(), OutboxState::Uncertain);
            assert_eq!(report.recovered_attempts(), 1);
            assert_eq!(
                report.recovered_execution_identity(),
                report.cut_execution_identity()
            );
            assert_eq!(report.recovered_observation_history_count(), 1);
            assert_eq!(
                report.recovered_evidence_code(),
                Some(EffectEvidenceCode::ProcessUncertain)
            );
            assert_eq!(report.recovered_not_started_reason(), None);
            assert_eq!(
                report.recovered_uncertainty(),
                Some(ProcessUncertaintyEvidence::SupervisorOutcomeLost)
            );
            assert!(!report.recovered_terminal_decision_present());
            assert_eq!(report.recovered_claim_marker_count(), 1);
            assert_eq!(report.recovered_spawn_permit_count(), 1);
            assert_eq!(report.recovered_release_authorization_count(), 1);
            assert_eq!(
                report.recovered_started_observed_marker_count(),
                started_observed
            );
            assert!(report.recovered_process_group_absent());
            assert_eq!(report.mission_status(), MissionStatus::InProgress);
            assert_eq!(report.phase_status(), PhaseStatus::Running);
            assert_eq!(report.event_types(), ["mission.started", "phase.started"]);
            assert_eq!(report.event_bytes(), report.cut_event_bytes());
            assert_eq!(report.checkpoint_bytes(), report.cut_checkpoint_bytes());
            if cell == CellKind::ProcessReleaseAuthorizedBeforeStarted {
                assert!(!report.cut_helper_executed());
                assert!(!report.helper_executed());
                assert_eq!(
                    report.first_recovery_disposition(),
                    Some(R0ProcessRecoveryDisposition::AuthorizedWithoutStartedCleanedUncertain)
                );
                assert_eq!(
                    report.second_recovery_disposition(),
                    Some(R0ProcessRecoveryDisposition::AuthorizedUncertainCleaned)
                );
            } else {
                assert_eq!(
                    report.first_recovery_disposition(),
                    Some(R0ProcessRecoveryDisposition::StartedProcessCleanedUncertain)
                );
                assert_eq!(
                    report.second_recovery_disposition(),
                    Some(R0ProcessRecoveryDisposition::StartedUncertainCleaned)
                );
            }
        }
        CellKind::ProcessPendingBeforeClaim => {
            assert_eq!(report.recovered_state(), OutboxState::Succeeded);
            assert_eq!(report.cut_state(), OutboxState::Pending);
            assert_eq!(report.cut_attempts(), 0);
            assert!(!report.cut_execution_identity_present());
            assert_eq!(report.cut_observation_history_count(), 0);
            assert!(!report.cut_terminal_decision_present());
            assert!(report.cut_event_types().is_empty());
            assert!(!report.cut_helper_executed());
            assert_eq!(
                raw_checkpoint_statuses_from_bytes(report.cut_checkpoint_bytes())?,
                ("pending".to_owned(), "pending".to_owned())
            );
        }
        CellKind::ProcessTerminalObservationBeforeDecision => {
            assert_eq!(report.recovered_state(), OutboxState::Succeeded);
            assert_eq!(report.cut_state(), OutboxState::Succeeded);
            assert_eq!(report.cut_attempts(), 1);
            assert!(report.cut_execution_identity_present());
            assert_eq!(report.cut_observation_history_count(), 1);
            assert!(!report.cut_terminal_decision_present());
            assert!(report.cut_event_types().is_empty());
            assert!(report.cut_helper_executed());
            assert_eq!(
                raw_checkpoint_statuses_from_bytes(report.cut_checkpoint_bytes())?,
                ("pending".to_owned(), "pending".to_owned())
            );
        }
        CellKind::ProcessTerminalDecisionBeforeProjection => {
            assert_eq!(report.recovered_state(), OutboxState::Succeeded);
            assert_eq!(report.cut_state(), OutboxState::Succeeded);
            assert_eq!(report.cut_attempts(), 1);
            assert!(report.cut_execution_identity_present());
            assert_eq!(report.cut_observation_history_count(), 1);
            assert!(report.cut_terminal_decision_present());
            assert_eq!(
                report.cut_event_types(),
                ["mission.started", "phase.started"]
            );
            assert!(report.cut_helper_executed());
            assert_eq!(
                raw_checkpoint_statuses_from_bytes(report.cut_checkpoint_bytes())?,
                ("in_progress".to_owned(), "running".to_owned())
            );
        }
        CellKind::MissionJournalBeforeProjection
        | CellKind::PhaseJournalBeforeProjection
        | CellKind::PhaseTerminalBeforeMissionTerminal
        | CellKind::EventAppendBeforeCheckpoint
        | CellKind::CheckpointFsyncBeforeReceipt
        | CellKind::MetricsCommitBeforeAcknowledgement => {
            return Err("non-process cell reached process recovery assertions".into());
        }
    }

    if matches!(
        cell,
        CellKind::ProcessPendingBeforeClaim
            | CellKind::ProcessTerminalObservationBeforeDecision
            | CellKind::ProcessTerminalDecisionBeforeProjection
    ) {
        assert_eq!(report.recovered_attempts(), 1);
        assert!(report.recovered_execution_identity_present());
        assert_eq!(report.recovered_observation_history_count(), 1);
        assert_eq!(
            report.recovered_evidence_code(),
            Some(EffectEvidenceCode::ExitObservedSuccess)
        );
        assert_eq!(report.recovered_not_started_reason(), None);
        assert_eq!(report.recovered_uncertainty(), None);
        assert!(report.recovered_terminal_decision_present());
        assert!(report.helper_executed());
        assert_eq!(report.mission_status(), MissionStatus::Completed);
        assert_eq!(report.phase_status(), PhaseStatus::Completed);
        assert_eq!(report.first_recovery_disposition(), None);
        assert_eq!(report.second_recovery_disposition(), None);
        assert!(!report.recovered_process_group_absent());
        assert_eq!(
            report.event_types(),
            [
                "mission.started",
                "phase.started",
                "worker.spawned",
                "worker.completed",
                "phase.completed",
                "mission.completed",
            ]
        );
        assert!(
            report.event_bytes().starts_with(report.cut_event_bytes()),
            "process recovery rewrote the durable event prefix"
        );
        assert_eq!(
            raw_checkpoint_statuses_from_bytes(report.checkpoint_bytes())?,
            ("completed".to_owned(), "completed".to_owned())
        );
    }
    Ok(())
}

fn restart_and_assert(cell: CellKind, root: &Path, parent: &Path) -> TestResult {
    if cell == CellKind::MetricsCommitBeforeAcknowledgement {
        return restart_metrics_and_assert(root);
    }
    if let Some(process_cell) = cell.process_cell() {
        return restart_process_and_assert(cell, process_cell, root, parent);
    }
    if cell == CellKind::PhaseJournalBeforeProjection {
        return restart_phase_start_and_assert(root, parent);
    }
    if cell == CellKind::PhaseTerminalBeforeMissionTerminal {
        return restart_phase_terminal_and_assert(root, parent);
    }
    let event_bytes_at_cut = std::fs::read(event_path(root))?;
    let checkpoint_bytes_at_cut = std::fs::read(checkpoint_path(root))?;
    let checkpoint_identity_at_cut = file_identity(&checkpoint_path(root))?;
    let journal_cell = cell
        .journal_cell()
        .ok_or("non-journal cell reached projector recovery")?;
    let first = recover_r0_journal_crash(recover_authority(root, parent)?, journal_cell)?;
    assert_report(cell, &first)?;
    assert_raw_projection(root, &first)?;
    assert_eq!(
        first.recovery_checkpoint_disposition(),
        cell.expected_disposition()
    );
    assert_eq!(std::fs::read(event_path(root))?, first.event_bytes());
    assert_eq!(
        std::fs::read(checkpoint_path(root))?,
        first.checkpoint_bytes()
    );

    match cell {
        CellKind::MissionJournalBeforeProjection => {
            assert!(event_bytes_at_cut.is_empty());
            assert_ne!(checkpoint_bytes_at_cut, first.checkpoint_bytes());
        }
        CellKind::EventAppendBeforeCheckpoint => {
            assert_eq!(event_bytes_at_cut, first.event_bytes());
            assert_ne!(checkpoint_bytes_at_cut, first.checkpoint_bytes());
        }
        CellKind::CheckpointFsyncBeforeReceipt => {
            assert_eq!(event_bytes_at_cut, first.event_bytes());
            assert_eq!(checkpoint_bytes_at_cut, first.checkpoint_bytes());
            assert_eq!(
                file_identity(&checkpoint_path(root))?,
                checkpoint_identity_at_cut,
                "cell 11 recovery replaced an already-target checkpoint"
            );
        }
        CellKind::ProcessPendingBeforeClaim
        | CellKind::ProcessClaimedBeforeSpecification
        | CellKind::ProcessReleaseAuthorizedBeforeStarted
        | CellKind::ProcessStartedObservedBeforeTerminal
        | CellKind::ProcessTerminalObservationBeforeDecision
        | CellKind::ProcessTerminalDecisionBeforeProjection
        | CellKind::PhaseJournalBeforeProjection
        | CellKind::PhaseTerminalBeforeMissionTerminal
        | CellKind::MetricsCommitBeforeAcknowledgement => unreachable!(),
    }

    let second = recover_r0_journal_crash(recover_authority(root, parent)?, journal_cell)?;
    assert_report(cell, &second)?;
    assert_raw_projection(root, &second)?;
    assert_eq!(second.recovery_checkpoint_disposition(), None);
    assert_eq!(second.event_bytes(), first.event_bytes());
    assert_eq!(second.checkpoint_bytes(), first.checkpoint_bytes());
    assert_eq!(std::fs::read(event_path(root))?, first.event_bytes());
    assert_eq!(
        std::fs::read(checkpoint_path(root))?,
        first.checkpoint_bytes()
    );
    if cell == CellKind::CheckpointFsyncBeforeReceipt {
        assert_eq!(
            file_identity(&checkpoint_path(root))?,
            checkpoint_identity_at_cut,
            "cell 11 stable reopen replaced checkpoint.json"
        );
    }
    Ok(())
}

// ===========================================================================
// Cell 12 — metrics committed before the acknowledgement
// ===========================================================================
//
// The B3 spine writes two databases and the pair is not one commit:
//
//   1. `RuntimeStore` journal transaction — phase transition + publication
//      intent, atomic.
//   2. `MetricsOwner::consume` -> `record_phase` — the real Go-compatible
//      `metrics.db` upsert, its own commit.
//   3. `RuntimeStore::resolve_publication(.. Delivered ..)` — the
//      acknowledgement, a *separate* commit.
//
// `metrics_audit_security_e2e`'s TRK-493 regression crashes at step 1. This
// cell crashes between step 2 and step 3, which is the only window where a
// committed `metrics.db` row exists with no acknowledgement behind it. The
// idempotence that has to save it — the `ON CONFLICT(id) DO UPDATE` upsert —
// was until now only ever exercised in-process by draining twice inside one
// test. Here the drain is cut by a real `SIGKILL`.
//
// The barrier is armed in the consumer closure this test composes, after
// `MetricsOwner::consume` has returned `Ok(())` and before
// `PublicationDrain::drain` reaches `resolve_publication`. That is the drain
// composition point, so no production file changes to obtain the cut.

fn metrics_manifests() -> Result<Vec<TypeManifest>, KnowledgeError> {
    Ok(vec![TypeManifest {
        namespace: Namespace::new(METRICS_NAMESPACE)?,
        type_name: TypeName::new(PHASE_METRIC_TYPE)?,
        kind: PrimitiveKind::Record,
        schema_versions: [SchemaVersion::new(1)?].into_iter().collect(),
        required_fields: BTreeSet::new(),
        sensitivity_ceiling: Sensitivity::Internal,
        allowed_operations: [
            Operation::writing(PrimitiveKind::Record),
            Operation::Get,
            Operation::Query,
        ]
        .into_iter()
        .collect(),
    }])
}

fn metrics_capability(
    generation: RegistryGeneration,
) -> Result<KnowledgeCapability, KnowledgeError> {
    Ok(KnowledgeCapability {
        namespace: Namespace::new(METRICS_NAMESPACE)?,
        types: [TypeName::new(PHASE_METRIC_TYPE)?].into_iter().collect(),
        operations: [Operation::Put, Operation::Get, Operation::Query]
            .into_iter()
            .collect(),
        field_mask: FieldMask::All,
        sensitivity_ceiling: Sensitivity::Internal,
        validity: Validity::at(generation),
        delegation: Delegation::NotDelegable,
    })
}

fn metrics_gateway() -> Result<AppKnowledgeGateway, KnowledgeError> {
    let registry = TypeRegistry::activate(metrics_manifests()?)?;
    let capability = metrics_capability(registry.generation())?;
    Ok(AppKnowledgeGateway::new(registry, capability))
}

/// Spawns the compiled fixture helper in its own process group, observes its
/// live kernel identity, reaps it, and returns the proof the group is gone.
///
/// `PhaseMetricIntent::new` cannot be called without one, which is the point:
/// a phase that left a background child alive has no representable metric.
fn metrics_reaped_witness() -> TestResult<ExactProcessGroupAbsence> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orchestrator-owned-process-fixture"));
    command
        .arg(METRICS_WITNESS_MODE)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let pid = child.id();
    // Block on the helper's own readiness frame, so the identity observation
    // below cannot race a process that has not finished starting.
    let stderr = child.stderr.take().ok_or("witness stderr is unavailable")?;
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            let _ = child.wait();
            return Err("metrics witness exited before announcing readiness".into());
        }
        if line.contains("\"ready\"") {
            break;
        }
    }
    let identity = KernelProcessIdentity::observe(pid, pid)?;
    // `stdin-race` reads to EOF and exits, so closing the pipe reaps it.
    drop(child.stdin.take());
    let status = child.wait()?;
    if !status.success() {
        return Err(format!("metrics witness exited abnormally: {status:?}").into());
    }
    match inspect_recorded_process_identity(
        identity.pid(),
        identity.process_group_id(),
        identity.process_start_identity(),
    )? {
        RecordedProcessIdentityStatus::ExactGroupAbsent(absence) => Ok(absence),
        other => Err(format!("expected a reaped group, observed {other:?}").into()),
    }
}

/// The exact phase metric cell 12 commits before it dies.
fn metrics_intent(witness: ExactProcessGroupAbsence) -> TestResult<PhaseMetricIntent> {
    let mut intent =
        PhaseMetricIntent::new(MissionId::new(METRICS_MISSION)?, METRICS_PHASE, 1, witness);
    intent.persona = "r0-metrics".to_owned();
    intent.selection_method = "explicit".to_owned();
    intent.status = MetricsPhaseStatus::Completed;
    intent.duration_s = 12;
    intent.gate_passed = true;
    intent.provider = "claude".to_owned();
    intent.model = "opus".to_owned();
    intent.tokens = TokenCounts {
        input: 11,
        output: 22,
        cache_creation: 3,
        cache_read: 4,
    };
    intent.cost_usd = 0.5;
    intent.worker_name = "r0-metrics-worker".to_owned();
    Ok(intent)
}

fn metrics_transition(mission: &MissionId) -> TestResult<JournalIntent> {
    Ok(JournalIntent::new(
        "phase.completed.metrics",
        Some(mission.clone()),
        "phase.completed",
        serde_json::json!({"step": 1}),
        METRICS_AT,
    )?
    .with_required_projection(CompatibilityProjection::Checkpoint))
}

/// Commits the journal transaction, drains it into `metrics.db`, and parks on
/// the barrier with the acknowledgement still uncommitted.
fn metrics_commit_child_main(cell: CellKind, parent: &Path) -> TestResult {
    let isolated = IsolatedFixtureRoot::create_fresh(parent)?;
    let root = isolated.path().to_path_buf();
    let mission = MissionId::new(METRICS_MISSION)?;
    let gateway = metrics_gateway()?;
    let intent = metrics_intent(metrics_reaped_witness()?)?;
    let payload = serde_json::to_string(&phase_metric_payload(&intent))?;
    let value: serde_json::Value = serde_json::from_str(&payload)?;
    let body = CanonicalJson::encode(&value)?;
    let canonical = body.as_str().to_owned();
    let envelope = RecordEnvelope::new(
        RecordId::new(format!(
            "{METRICS_NAMESPACE}/{PHASE_METRIC_TYPE}/{METRICS_PHASE}"
        ))?,
        RevisionId::GENESIS,
        ExpectedHead::absent(),
        Governance {
            namespace: Namespace::new(METRICS_NAMESPACE)?,
            type_name: TypeName::new(PHASE_METRIC_TYPE)?,
            schema_version: SchemaVersion::new(1)?,
            provenance: Provenance::new("r0-kill9-restart-matrix", Some(METRICS_MISSION))?,
            sensitivity: Sensitivity::Internal,
            lifecycle: LifecycleState::Active,
            registry_generation: gateway.registry().generation(),
        },
        body,
    );
    let evidence = gateway.admit(envelope)?;
    let publication =
        gateway.publication_intent(&mission, Some(METRICS_PHASE), 1, &evidence, &canonical)?;

    let mut store = open_fixture_runtime_store(&isolated)?;
    store.append(&gateway.attach(metrics_transition(&mission)?, publication)?)?;

    let owner = MetricsOwner::assume(MetricsOwnerCapability::in_fixture_boundary(
        fixture_production_boundary(&isolated)?,
    )?)?;
    let drain = publication_drain();
    let ack_root = root.clone();
    // `consume` commits the `metrics.db` upsert. The barrier fires on the very
    // next statement, so `resolve_publication` is never reached: the parent
    // `SIGKILL`s this process while it is parked here.
    drain.drain(&mut store, 16, METRICS_AT, |claimed| {
        match owner.consume(claimed) {
            Ok(()) => durable_actor_barrier_ack_and_park(cell, &ack_root),
            Err(_) => DeliveryVerdict::Refused,
        }
    })?;
    Err("cell 12 drain returned without SIGKILL".into())
}

/// Restarts on the crashed home and proves the split commit converges.
fn restart_metrics_and_assert(root: &Path) -> TestResult {
    let isolated = IsolatedFixtureRoot::identify(root)?;
    let mut store = open_fixture_runtime_store(&isolated)?;
    let owner = MetricsOwner::assume(MetricsOwnerCapability::in_fixture_boundary(
        fixture_production_boundary(&isolated)?,
    )?)?;

    // The `metrics.db` commit survived a process that never unwound.
    let before: RecordedPhase = owner
        .phase_row(METRICS_MISSION, METRICS_PHASE)?
        .ok_or("the pre-crash metrics row must have survived the SIGKILL")?;
    assert_eq!(before.status, "completed");
    assert!(before.gate_passed);
    assert_eq!(
        owner.recorded_phase_names(METRICS_MISSION)?,
        vec![METRICS_PHASE.to_owned()],
        "exactly one phase row may exist before recovery",
    );

    // The acknowledgement did not commit: claiming only bumps `attempts`, so
    // the row is still pending and recovery must redeliver it.
    let counts = store.publication_counts()?;
    assert_eq!(
        counts.pending, 1,
        "the unacknowledged publication must still be pending: {counts:?}",
    );
    assert_eq!(counts.delivered, 0, "{counts:?}");
    assert_eq!(counts.dead_letter, 0, "{counts:?}");

    let drain = publication_drain();
    let mut consume_error: Option<String> = None;
    let report = drain.drain(&mut store, 16, METRICS_AT, |claimed| {
        match owner.consume(claimed) {
            Ok(()) => DeliveryVerdict::Accepted,
            Err(e) => {
                consume_error = Some(e.to_string());
                DeliveryVerdict::Refused
            }
        }
    })?;
    assert!(
        consume_error.is_none(),
        "recovery consume must not refuse: {consume_error:?}",
    );
    assert_eq!(report.delivered.len(), 1, "{report:?}");
    assert!(report.dead_lettered.is_empty(), "{report:?}");

    // Redelivery converged on the identical row rather than duplicating it.
    assert_eq!(
        owner.phase_row(METRICS_MISSION, METRICS_PHASE)?,
        Some(before),
        "redelivery rewrote the pre-crash row with different values",
    );
    assert_eq!(
        owner.recorded_phase_names(METRICS_MISSION)?,
        vec![METRICS_PHASE.to_owned()],
        "redelivery duplicated the phase row",
    );
    let counts = store.publication_counts()?;
    assert_eq!(counts.pending, 0, "{counts:?}");
    assert_eq!(counts.delivered, 1, "{counts:?}");
    assert_eq!(counts.dead_letter, 0, "{counts:?}");

    // A second pass finds nothing and changes nothing.
    let second = drain.drain(&mut store, 16, METRICS_AT, |_| DeliveryVerdict::Accepted)?;
    assert!(second.delivered.is_empty(), "{second:?}");
    assert!(second.dead_lettered.is_empty(), "{second:?}");
    assert_eq!(store.publication_counts()?.pending, 0);
    store.close()?;
    Ok(())
}

struct SpawnedChild {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Drop for SpawnedChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            if let Some(group) = checked_pid(self.child.id()) {
                let _ = kill_process_group(group, Signal::KILL);
            }
            let _ = self.child.wait();
        }
    }
}

fn spawn_crash_child(
    cell: CellKind,
    parent: &Path,
    runner: &Path,
    test_name: &str,
) -> TestResult<SpawnedChild> {
    let mut command = Command::new(runner);
    command
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(CHILD_ROLE_ENV, "crash")
        .env(CHILD_CELL_ENV, cell.name())
        .env(CHILD_PARENT_ENV, parent)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().ok_or("child stdout is unavailable")?;
    let (sender, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    Ok(SpawnedChild { child, lines })
}

fn spawn_phase_start_worker_projection_cut_child(
    parent: &Path,
    runner: &Path,
    root: &Path,
    test_name: &str,
) -> TestResult<SpawnedChild> {
    let mut command = Command::new(runner);
    command
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(CHILD_ROLE_ENV, CELL2_WORKER_PROJECTION_CUT_ROLE)
        .env(
            CHILD_CELL_ENV,
            CellKind::PhaseJournalBeforeProjection.name(),
        )
        .env(CHILD_PARENT_ENV, parent)
        .env(CHILD_ROOT_ENV, root)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().ok_or("child stdout is unavailable")?;
    let (sender, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    Ok(SpawnedChild { child, lines })
}

fn kill_phase_start_worker_projection_cut(
    parent: &Path,
    runner: &Path,
    root: &Path,
    test_name: &str,
) -> TestResult<FileIdentity> {
    let mut spawned =
        spawn_phase_start_worker_projection_cut_child(parent, runner, root, test_name)?;
    let child_pid = spawned.child.id();
    let acknowledged_root =
        wait_for_durable_ack(&mut spawned, CellKind::PhaseJournalBeforeProjection)?;
    if acknowledged_root != root {
        return Err("Cell 2 worker-projection child acknowledged a different root".into());
    }

    let (sentinel, duplicate) = phase_start_helper_marker_paths(root)?;
    assert_eq!(std::fs::read(&sentinel)?, DURABLE_CANARY_CONTENT);
    require_path_absent(&duplicate)?;
    let sentinel_identity = file_identity(&sentinel)?;

    let pid = checked_pid(child_pid).ok_or("Cell 2 worker-projection child PID is invalid")?;
    kill_process(pid, Signal::KILL)?;
    let status = spawned.child.wait()?;
    assert_eq!(status.signal(), Some(Signal::KILL.as_raw()));
    assert_process_group_absent(child_pid)?;
    Ok(sentinel_identity)
}

fn run_recovery_child(
    cell: CellKind,
    parent: &Path,
    runner: &Path,
    root: &Path,
    test_name: &str,
) -> TestResult {
    let mut child = Command::new(runner)
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(CHILD_ROLE_ENV, "recover")
        .env(CHILD_CELL_ENV, cell.name())
        .env(CHILD_PARENT_ENV, parent)
        .env(CHILD_ROOT_ENV, root)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    let recovery_group = child.id();
    let status = child.wait()?;
    if !status.success() {
        return Err(format!("{} recovery process failed with {status}", cell.name()).into());
    }
    assert_process_group_absent(recovery_group)
}

fn wait_for_durable_ack(spawned: &mut SpawnedChild, cell: CellKind) -> TestResult<PathBuf> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("{} did not acknowledge its durable cut", cell.name()).into());
        }
        match spawned.lines.recv_timeout(remaining) {
            Ok(line) => {
                if let Some(rest) = line.strip_prefix(&format!("{ACK_PREFIX} {} ", cell.name())) {
                    return Ok(PathBuf::from(rest));
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(format!("{} durable acknowledgement timed out", cell.name()).into());
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let status = spawned.child.try_wait()?;
                return Err(format!(
                    "{} child exited before acknowledgement: {status:?}",
                    cell.name()
                )
                .into());
            }
        }
    }
}

fn assert_process_group_absent(raw: u32) -> TestResult {
    let pgid = checked_pid(raw).ok_or("child process-group ID is invalid")?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match test_kill_process_group(pgid) {
            Err(rustix::io::Errno::SRCH) => return Ok(()),
            Ok(()) if Instant::now() < deadline => std::thread::yield_now(),
            Ok(()) => return Err(format!("child process group {raw} survived SIGKILL").into()),
            Err(error) => return Err(error.into()),
        }
    }
}

fn run_parent_cell(cell: CellKind, test_name: &str) -> TestResult {
    let temporary = std::fs::canonicalize(std::env::temp_dir())?;
    let parent = temporary.join(format!(
        "orchestrator-r0-kill9-{}-{}",
        std::process::id(),
        cell.name()
    ));
    remove_private_tree(&parent)?;
    private_dir(&parent)?;
    let runner = materialize_private_controller(&parent)?;

    let mut spawned = spawn_crash_child(cell, &parent, &runner, test_name)?;
    let child_pid = spawned.child.id();
    let root = wait_for_durable_ack(&mut spawned, cell)?;
    let pid = checked_pid(child_pid).ok_or("child PID is invalid")?;
    kill_process(pid, Signal::KILL)?;
    let status = spawned.child.wait()?;
    assert_eq!(status.signal(), Some(Signal::KILL.as_raw()));

    let phase_start_sentinel_identity = if cell == CellKind::PhaseJournalBeforeProjection {
        Some(kill_phase_start_worker_projection_cut(
            &parent, &runner, &root, test_name,
        )?)
    } else {
        None
    };
    run_recovery_child(cell, &parent, &runner, &root, test_name)?;
    if let Some(expected_identity) = phase_start_sentinel_identity {
        let (sentinel, duplicate) = phase_start_helper_marker_paths(&root)?;
        assert_eq!(std::fs::read(&sentinel)?, DURABLE_CANARY_CONTENT);
        assert_eq!(
            file_identity(&sentinel)?,
            expected_identity,
            "Cell 2 recovery replaced the exact helper sentinel"
        );
        require_path_absent(&duplicate)?;
    }
    assert_process_group_absent(child_pid)?;
    remove_private_tree(&parent)?;
    Ok(())
}

fn run_matrix_cell(expected: CellKind, test_name: &str) -> TestResult {
    if let Some(role) = std::env::var_os(CHILD_ROLE_ENV) {
        if std::env::var(CHILD_CELL_ENV).as_deref() != Ok(expected.name()) {
            return Err("R0 child cell does not match the exact test".into());
        }
        let parent = std::env::var_os(CHILD_PARENT_ENV)
            .map(PathBuf::from)
            .ok_or("R0 child parent is missing")?;
        return match role.to_str() {
            Some("crash") => child_main(expected, &parent),
            Some(CELL2_WORKER_PROJECTION_CUT_ROLE)
                if expected == CellKind::PhaseJournalBeforeProjection =>
            {
                let root = std::env::var_os(CHILD_ROOT_ENV)
                    .map(PathBuf::from)
                    .ok_or("Cell 2 worker-projection root is missing")?;
                phase_start_worker_projection_cut_main(&root, &parent)
            }
            Some("recover") => {
                let root = std::env::var_os(CHILD_ROOT_ENV)
                    .map(PathBuf::from)
                    .ok_or("R0 recovery root is missing")?;
                restart_and_assert(expected, &root, &parent)
            }
            _ => Err("R0 child role is invalid".into()),
        };
    }
    run_parent_cell(expected, test_name)
}

macro_rules! define_r0_restart_matrix {
    (
        $(
            $number:literal => $test_name:ident $body:block
        )+
    ) => {
        const _: [(); 12] = [(); [$($number),+].len()];
        const _: () = {
            let cell_inventory: &[u8] = &[$($number),+];
            let mut index = 0;
            while index < cell_inventory.len() {
                assert!(
                    cell_inventory[index] == (index + 1) as u8,
                    "R0 restart-cell inventory must contain exactly cells 1 through 12"
                );
                index += 1;
            }
        };

        $(
            #[test]
            fn $test_name() -> TestResult $body
        )+
    };
}

define_r0_restart_matrix! {
    1 => cell_01_mission_journal_before_projection {
        run_matrix_cell(
            CellKind::MissionJournalBeforeProjection,
            "cell_01_mission_journal_before_projection",
        )
    }
    2 => cell_02_phase_journal_before_worker_projection {
        run_matrix_cell(
            CellKind::PhaseJournalBeforeProjection,
            "cell_02_phase_journal_before_worker_projection",
        )
    }
    3 => cell_03_process_pending_before_claim {
        run_matrix_cell(
            CellKind::ProcessPendingBeforeClaim,
            "cell_03_process_pending_before_claim",
        )
    }
    4 => cell_04_claim_before_spec_init {
        run_matrix_cell(
            CellKind::ProcessClaimedBeforeSpecification,
            "cell_04_claim_before_spec_init",
        )
    }
    5 => cell_05_release_authorization_before_spawn_observation {
        run_matrix_cell(
            CellKind::ProcessReleaseAuthorizedBeforeStarted,
            "cell_05_release_authorization_before_spawn_observation",
        )
    }
    6 => cell_06_spawned_observation_before_terminal_observation {
        run_matrix_cell(
            CellKind::ProcessStartedObservedBeforeTerminal,
            "cell_06_spawned_observation_before_terminal_observation",
        )
    }
    7 => cell_07_terminal_observation_before_terminal_decision {
        run_matrix_cell(
            CellKind::ProcessTerminalObservationBeforeDecision,
            "cell_07_terminal_observation_before_terminal_decision",
        )
    }
    8 => cell_08_terminal_decision_before_worker_checkpoint {
        run_matrix_cell(
            CellKind::ProcessTerminalDecisionBeforeProjection,
            "cell_08_terminal_decision_before_worker_checkpoint",
        )
    }
    9 => cell_09_phase_terminal_before_mission_terminal {
        run_matrix_cell(
            CellKind::PhaseTerminalBeforeMissionTerminal,
            "cell_09_phase_terminal_before_mission_terminal",
        )
    }
    10 => cell_10_event_append_before_checkpoint {
        run_matrix_cell(
            CellKind::EventAppendBeforeCheckpoint,
            "cell_10_event_append_before_checkpoint",
        )
    }
    11 => cell_11_checkpoint_fsync_before_receipt {
        run_matrix_cell(
            CellKind::CheckpointFsyncBeforeReceipt,
            "cell_11_checkpoint_fsync_before_receipt",
        )
    }
    12 => cell_12_metrics_commit_before_acknowledgement {
        run_matrix_cell(
            CellKind::MetricsCommitBeforeAcknowledgement,
            "cell_12_metrics_commit_before_acknowledgement",
        )
    }
}
