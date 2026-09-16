//! Verifies the `orchestrator hermetic-canary` command surface: it is
//! refused without the opt-in env or the run root, and runs the durable
//! canary composition when both are set.
#![cfg(all(unix, feature = "verification-process-canary"))]

use std::{path::Path, process::Command};

use orchestrator_cli::run_system;
use rusqlite::{Connection, OpenFlags, params};

const OPT_IN_ENV: &str = "NANIKA_HERMETIC_PROCESS_CANARY";
const RUN_ROOT_ENV: &str = "NANIKA_HERMETIC_RUN_ROOT";

#[cfg(target_os = "macos")]
#[derive(Debug, Eq, PartialEq)]
struct FileMetadataSnapshot {
    device: u64,
    inode: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    length: u64,
    mode: u32,
    owner: u32,
    links: u64,
}

#[cfg(target_os = "macos")]
fn file_metadata_snapshot(path: &Path) -> Result<FileMetadataSnapshot, String> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(path)
        .map_err(|source| format!("inspect replay artifact metadata: {source}"))?;
    Ok(FileMetadataSnapshot {
        device: metadata.dev(),
        inode: metadata.ino(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        length: metadata.len(),
        mode: metadata.mode(),
        owner: metadata.uid(),
        links: metadata.nlink(),
    })
}

#[derive(Debug, Eq, PartialEq)]
struct CanaryLedgerSummary {
    outbox_rows: i64,
    idempotency_key: String,
    state: String,
    attempts: i64,
    logical_attempt: i64,
    claim_rows: i64,
    execution_identity_rows: i64,
    release_authorization_rows: i64,
    observation_rows: i64,
    observation_state: String,
    terminal_decision_rows: i64,
}

fn canary_ledger_summary(path: &Path) -> Result<CanaryLedgerSummary, String> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|source| format!("open canary ledger read-only: {source}"))?;
    let binding = params![
        "canary-mission",
        "canary-phase",
        "provider_process",
        "hermetic-provider-attempt",
        1_i64,
    ];
    let outbox_rows = connection
        .query_row(
            "SELECT COUNT(*) FROM outbox
             WHERE mission_id = ?1 AND phase_id = ?2 AND effect_kind = ?3
               AND operation_slot = ?4 AND logical_attempt = ?5",
            binding,
            |row| row.get(0),
        )
        .map_err(|source| format!("count exact canary outbox rows: {source}"))?;
    let (idempotency_key, state, attempts, logical_attempt) = connection
        .query_row(
            "SELECT idempotency_key, state, attempts, logical_attempt FROM outbox
             WHERE mission_id = ?1 AND phase_id = ?2 AND effect_kind = ?3
               AND operation_slot = ?4 AND logical_attempt = ?5",
            params![
                "canary-mission",
                "canary-phase",
                "provider_process",
                "hermetic-provider-attempt",
                1_i64,
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|source| format!("read exact canary outbox row: {source}"))?;
    let claim_rows = connection
        .query_row(
            "SELECT COUNT(*) FROM outbox_attempt_claim WHERE idempotency_key = ?1",
            params![&idempotency_key],
            |row| row.get(0),
        )
        .map_err(|source| format!("count canary claims: {source}"))?;
    let execution_identity_rows = connection
        .query_row(
            "SELECT COUNT(*) FROM outbox_execution_identity WHERE idempotency_key = ?1",
            params![&idempotency_key],
            |row| row.get(0),
        )
        .map_err(|source| format!("count canary execution identities: {source}"))?;
    let release_authorization_rows = connection
        .query_row(
            "SELECT COUNT(*) FROM outbox_process_release_authorization
             WHERE idempotency_key = ?1",
            params![&idempotency_key],
            |row| row.get(0),
        )
        .map_err(|source| format!("count canary release authorizations: {source}"))?;
    let (observation_rows, observation_state) = connection
        .query_row(
            "SELECT COUNT(*), MIN(observed_state) FROM outbox_attempt_observation
             WHERE idempotency_key = ?1",
            params![&idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|source| format!("read canary observations: {source}"))?;
    let terminal_decision_rows = connection
        .query_row(
            "SELECT COUNT(*) FROM journal
             WHERE mission_id = ?1 AND transition_kind = 'cell1.terminal_decision'",
            params!["canary-mission"],
            |row| row.get(0),
        )
        .map_err(|source| format!("count canary terminal decisions: {source}"))?;
    Ok(CanaryLedgerSummary {
        outbox_rows,
        idempotency_key,
        state,
        attempts,
        logical_attempt,
        claim_rows,
        execution_identity_rows,
        release_authorization_rows,
        observation_rows,
        observation_state,
        terminal_decision_rows,
    })
}

#[test]
fn hermetic_canary_is_refused_without_opt_in_env() -> Result<(), String> {
    if std::env::var_os(OPT_IN_ENV).is_some() {
        return Err(format!(
            "{OPT_IN_ENV} must not be set in the test environment"
        ));
    }
    let mut output = Vec::new();
    let mut error_output = Vec::new();
    let result = run_system(
        ["hermetic-canary".to_owned()],
        &mut output,
        &mut error_output,
    );
    let error = match result {
        Err(error) => error,
        Ok(()) => return Err("canary must refuse without the opt-in env".to_owned()),
    };
    let message = format!("{error:?}");
    assert!(
        message.contains(OPT_IN_ENV),
        "error must name the opt-in env: {message}"
    );
    assert!(
        output.is_empty(),
        "no output before the env gate: {output:?}"
    );
    Ok(())
}

#[test]
fn hermetic_canary_is_refused_without_run_root() -> Result<(), String> {
    // opt-in set but run root missing → rejected before mutation.
    let bin = env!("CARGO_BIN_EXE_orchestrator");
    let output = Command::new(bin)
        .arg("hermetic-canary")
        .env(OPT_IN_ENV, "1")
        .env_remove(RUN_ROOT_ENV)
        .output()
        .map_err(|source| format!("orchestrator binary should spawn: {source}"))?;
    assert!(
        !output.status.success(),
        "canary must fail without run root: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(RUN_ROOT_ENV),
        "error must name the run root env: {stderr}"
    );
    Ok(())
}

#[test]
#[cfg(target_os = "macos")]
fn hermetic_canary_durable_run_and_replay_do_not_relaunch() -> Result<(), String> {
    // The canonical `scripts/verify-fast-cli.sh` gate builds the exact
    // companion process broker before this integration binary runs.
    let bin = env!("CARGO_BIN_EXE_orchestrator");
    let run_root = std::env::temp_dir().join(format!(
        "orchestrator-canary-replay-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let run_root_str = run_root.to_str().ok_or("non-utf8 run root path")?;

    // First run: fresh — should succeed.
    let output = Command::new(bin)
        .arg("hermetic-canary")
        .env(OPT_IN_ENV, "1")
        .env(RUN_ROOT_ENV, run_root_str)
        .output()
        .map_err(|source| format!("first canary run should spawn: {source}"))?;
    assert!(
        output.status.success(),
        "first canary run should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hermetic process canary: succeeded"),
        "fresh canary must report a proven successful process: {stdout}"
    );
    assert!(
        stdout.contains("spawned:       yes"),
        "fresh canary must report its durably identified kernel spawn: {stdout}"
    );
    let sentinel = run_root
        .join("targets/compat/workspaces/canary-mission/workers/canary-canary-phase/sentinel.txt");
    assert_eq!(
        std::fs::read(&sentinel).map_err(|source| format!("read canary sentinel: {source}"))?,
        b"nanika-hermetic-canary-sentinel-v1\n"
    );
    let sentinel_before = std::fs::symlink_metadata(&sentinel)
        .map_err(|source| format!("inspect canary sentinel before replay: {source}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(sentinel_before.mode() & 0o777, 0o600);
    }
    let ledger = run_root.join("targets/private-ledger/runtime.db");
    let checkpoint = run_root.join("targets/compat/workspaces/canary-mission/checkpoint.json");
    let events = run_root.join("targets/compat/workspaces/canary-mission/events.jsonl");
    let ledger_before = canary_ledger_summary(&ledger)?;
    assert_eq!(ledger_before.outbox_rows, 1);
    assert_eq!(ledger_before.state, "succeeded");
    assert_eq!(ledger_before.attempts, 1);
    assert_eq!(ledger_before.logical_attempt, 1);
    assert_eq!(ledger_before.claim_rows, 1);
    assert_eq!(ledger_before.execution_identity_rows, 1);
    assert_eq!(ledger_before.release_authorization_rows, 1);
    assert_eq!(ledger_before.observation_rows, 1);
    assert_eq!(ledger_before.observation_state, "succeeded");
    assert_eq!(ledger_before.terminal_decision_rows, 1);
    let checkpoint_before = std::fs::read(&checkpoint)
        .map_err(|source| format!("read checkpoint before replay: {source}"))?;
    let events_before =
        std::fs::read(&events).map_err(|source| format!("read events before replay: {source}"))?;
    let checkpoint_metadata_before = file_metadata_snapshot(&checkpoint)?;
    let events_metadata_before = file_metadata_snapshot(&events)?;
    let decoded_checkpoint = orchestrator_core::decode_checkpoint(&checkpoint_before)
        .map_err(|source| format!("decode terminal checkpoint: {source}"))?;
    assert_eq!(
        decoded_checkpoint.projection.status, "completed",
        "fresh canary must persist a completed mission checkpoint"
    );
    let projected_phase = decoded_checkpoint
        .projection
        .plan
        .as_ref()
        .and_then(|plan| plan.phases.iter().find(|phase| phase.id == "canary-phase"))
        .ok_or("terminal checkpoint is missing the canary phase")?;
    assert_eq!(
        projected_phase.status, "completed",
        "fresh canary must persist a completed phase checkpoint"
    );
    let event_types = events_before
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            orchestrator_core::decode_event_line(line)
                .map(|decoded| decoded.record.event_type)
                .map_err(|source| format!("decode terminal event: {source}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let phase_completed = event_types
        .iter()
        .position(|event_type| event_type == "phase.completed")
        .ok_or("fresh canary events are missing phase.completed")?;
    let mission_completed = event_types
        .iter()
        .position(|event_type| event_type == "mission.completed")
        .ok_or("fresh canary events are missing mission.completed")?;
    assert!(
        phase_completed < mission_completed,
        "phase completion must precede mission completion: {event_types:?}"
    );
    assert_eq!(
        event_types.last().map(String::as_str),
        Some("mission.completed"),
        "mission.completed must be the final lifecycle event"
    );

    // Second run: the same provider admission must prove the existing
    // terminal decision and projection without spawning the helper again.
    let output2 = Command::new(bin)
        .arg("hermetic-canary")
        .env(OPT_IN_ENV, "1")
        .env(RUN_ROOT_ENV, run_root_str)
        .output()
        .map_err(|source| format!("replay canary run should spawn: {source}"))?;
    assert!(
        output2.status.success(),
        "replay should succeed from durable authority: {}",
        String::from_utf8_lossy(&output2.stderr)
    );
    let stdout2 = String::from_utf8_lossy(&output2.stdout);
    assert!(
        stdout2.contains("hermetic process canary: succeeded"),
        "replay must preserve the successful terminal decision: {stdout2}"
    );
    assert!(
        stdout2.contains("spawned:       no (terminal replay)"),
        "replay must report that no helper was launched: {stdout2}"
    );

    let identity_lines = |rendered: &str| {
        rendered
            .lines()
            .filter(|line| {
                line.starts_with("  leaf:")
                    || line.starts_with("  helper:")
                    || line.starts_with("  mission:")
                    || line.starts_with("  worker:")
                    || line.starts_with("  executable id:")
            })
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        identity_lines(&stdout),
        identity_lines(&stdout2),
        "replay must report the identical durable mission/helper identity"
    );
    assert_eq!(
        std::fs::read(&sentinel)
            .map_err(|source| format!("read canary sentinel after replay: {source}"))?,
        b"nanika-hermetic-canary-sentinel-v1\n"
    );
    let sentinel_after = std::fs::symlink_metadata(&sentinel)
        .map_err(|source| format!("inspect canary sentinel after replay: {source}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            (
                sentinel_after.dev(),
                sentinel_after.ino(),
                sentinel_after.mtime(),
                sentinel_after.mtime_nsec(),
            ),
            (
                sentinel_before.dev(),
                sentinel_before.ino(),
                sentinel_before.mtime(),
                sentinel_before.mtime_nsec(),
            ),
            "replay must not replace or rewrite the sentinel"
        );
    }
    assert_eq!(
        canary_ledger_summary(&ledger)?,
        ledger_before,
        "replay must preserve the exact logical process-attempt summary"
    );
    assert_eq!(
        std::fs::read(&checkpoint)
            .map_err(|source| format!("read checkpoint after replay: {source}"))?,
        checkpoint_before,
        "replay must not advance the terminal checkpoint again"
    );
    assert_eq!(
        std::fs::read(&events).map_err(|source| format!("read events after replay: {source}"))?,
        events_before,
        "replay must not append duplicate lifecycle events"
    );
    assert_eq!(
        file_metadata_snapshot(&checkpoint)?,
        checkpoint_metadata_before,
        "replay must not replace, rewrite, relink, rechown, or chmod the checkpoint"
    );
    assert_eq!(
        file_metadata_snapshot(&events)?,
        events_metadata_before,
        "replay must not replace, rewrite, relink, rechown, or chmod the event log"
    );

    let _ = std::fs::remove_dir_all(&run_root);
    Ok(())
}
