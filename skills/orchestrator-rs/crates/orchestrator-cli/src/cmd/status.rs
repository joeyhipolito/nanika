//! Ports Go's `orchestrator status` command (`internal/cmd/status.go`).
//!
//! `showRunningMissions` scans `<home>/events/*.jsonl` for missions whose
//! projected status is `"in_progress"`. `showStatus` then lists the five
//! most recently created workspaces under `<home>/workspaces/`, preferring
//! event-log-derived phase-completion counts over the checkpoint's own when
//! available. Both event-log reads go through
//! [`orchestrator_exec::project_mission_in_bytes`] — the pure, byte-in
//! projection function — after this module reads the file itself via plain
//! `std::fs` (see `cmd/mod.rs` module doc comment for why `Dir`-based
//! wrappers are not used here).

use std::{
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use orchestrator_core::decode_checkpoint;
use orchestrator_exec::{MissionSnap, PhaseSnap, project_mission_in_bytes};
use thiserror::Error;

use super::time_fmt::{format_go_duration_seconds, format_unix_seconds_ymd_hms};

/// Maximum bytes of `taskSummary` shown per workspace, matching Go's
/// `if len(taskSummary) > 80 { taskSummary = taskSummary[:80] + "..." }`
/// (`internal/cmd/status.go:59-61`).
const TASK_SUMMARY_MAX_BYTES: usize = 80;
/// `internal/cmd/status.go:45`: "Show last 5".
const MAX_WORKSPACES_SHOWN: usize = 5;

#[derive(Debug, Error)]
pub(crate) enum StatusError {
    #[error("reading workspaces directory: {0}")]
    ReadWorkspacesDir(#[source] std::io::Error),
    #[error("cannot write command output: {0}")]
    Output(#[from] std::io::Error),
}

/// Runs `orchestrator status` against `home`, writing to `output`
/// (Go's `cmd.OutOrStdout()`/plain `fmt.Println`, both stdout in this CLI)
/// and `error_output` (Go's `cmd.ErrOrStderr()`, plain stderr).
pub(crate) fn run(
    home: &Path,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), StatusError> {
    show_running_missions(home, output, error_output)?;

    let workspaces = list_workspaces(home)?;
    if workspaces.is_empty() {
        writeln!(output, "no workspaces found")?;
        return Ok(());
    }

    for workspace_path in workspaces.iter().take(MAX_WORKSPACES_SHOWN) {
        print_workspace_status(workspace_path, output, error_output)?;
    }
    Ok(())
}

fn print_workspace_status(
    workspace_path: &Path,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), StatusError> {
    let Ok(checkpoint_bytes) = std::fs::read(workspace_path.join("checkpoint.json")) else {
        return Ok(()); // Go: `if err != nil { continue }` (checkpoint.go:110-113 read failure).
    };
    let Ok(checkpoint) = decode_checkpoint(&checkpoint_bytes) else {
        return Ok(()); // Go: LoadCheckpoint parse failure -> `continue`.
    };
    let projection = checkpoint.projection;

    let mission_text =
        std::fs::read_to_string(workspace_path.join("mission.md")).unwrap_or_default();
    let task_summary = truncate_task_summary(mission_text.trim());

    let total = projection.plan.as_ref().map_or(0, |plan| plan.phases.len());

    let mut status = projection.status.clone();

    let workspace_id = projection.workspace_id.as_str();
    let completed = match std::fs::read(event_log_path(workspace_path, workspace_id)) {
        Ok(bytes) => match project_mission_in_bytes(&bytes, workspace_id) {
            Ok(Some(snap)) => {
                if status.is_empty() || status == "in_progress" {
                    status = snap.status.clone();
                }
                snap.phases
                    .values()
                    .filter(|phase| phase.status == "completed")
                    .count()
            }
            Ok(None) => checkpoint_completed_count(&projection),
            Err(error) => {
                warn_live_status_unavailable(workspace_path, &error, error_output)?;
                checkpoint_completed_count(&projection)
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            checkpoint_completed_count(&projection)
        }
        Err(error) => {
            warn_live_status_unavailable(workspace_path, &EventLogReadError(error), error_output)?;
            checkpoint_completed_count(&projection)
        }
    };

    let issue_tag = read_sidecar_trimmed(workspace_path, "linear_issue_id")
        .filter(|value| !value.is_empty())
        .map(|value| format!(" ({value})"))
        .unwrap_or_default();

    let workspace_base = workspace_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();

    writeln!(
        output,
        "{workspace_base} [{status}] {completed}/{total} phases{issue_tag} — {task_summary}"
    )?;

    if let Some(pr_url) = read_sidecar_trimmed(workspace_path, "pr_url") {
        if !pr_url.is_empty() {
            writeln!(output, "  PR: {pr_url}")?;
        }
    }
    Ok(())
}

struct EventLogReadError(std::io::Error);

impl std::fmt::Display for EventLogReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

fn warn_live_status_unavailable(
    workspace_path: &Path,
    error: &impl std::fmt::Display,
    error_output: &mut impl Write,
) -> Result<(), StatusError> {
    let workspace_base = workspace_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    writeln!(
        error_output,
        "warning: live status unavailable for {workspace_base}: {error}"
    )?;
    Ok(())
}

fn checkpoint_completed_count(projection: &orchestrator_core::CheckpointProjection) -> usize {
    projection
        .plan
        .as_ref()
        .map(|plan| {
            plan.phases
                .iter()
                .filter(|phase| phase.status == "completed")
                .count()
        })
        .unwrap_or(0)
}

fn event_log_path(workspace_path: &Path, workspace_id: &str) -> PathBuf {
    // `<home>/workspaces/<id>/../../events/<id>.jsonl` — mirrors
    // `event.EventLogPath` (`<config.Dir()>/events/<missionID>.jsonl`) via the
    // shared `<home>` two levels up from a workspace directory
    // (`<home>/workspaces/<id>`).
    workspace_path.parent().and_then(Path::parent).map_or_else(
        || PathBuf::from("events").join(format!("{workspace_id}.jsonl")),
        |home| home.join("events").join(format!("{workspace_id}.jsonl")),
    )
}

fn read_sidecar_trimmed(workspace_path: &Path, file_name: &str) -> Option<String> {
    std::fs::read_to_string(workspace_path.join(file_name))
        .ok()
        .map(|contents| contents.trim().to_owned())
}

/// Ports the byte-index truncation in `showStatus`
/// (`internal/cmd/status.go:59-61`); see `cmd/mod.rs`'s `truncate` doc
/// comment for the same char-boundary caveat (this uses its own constant and
/// suffix placement, so it is not routed through that shared helper).
fn truncate_task_summary(task: &str) -> String {
    if task.len() <= TASK_SUMMARY_MAX_BYTES {
        return task.to_owned();
    }
    let mut cut = TASK_SUMMARY_MAX_BYTES;
    while cut > 0 && !task.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}...", &task[..cut])
}

/// Ports `core.ListWorkspaces` (`internal/core/workspace.go:163-195`):
/// directories under `<home>/workspaces` containing a `mission.md`,
/// newest-first (IDs are timestamp-prefixed, so a descending name sort
/// matches Go's ascending-then-reversed `os.ReadDir` order).
fn list_workspaces(home: &Path) -> Result<Vec<PathBuf>, StatusError> {
    let workspaces_dir = home.join("workspaces");
    let entries = match std::fs::read_dir(&workspaces_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(StatusError::ReadWorkspacesDir(error)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(StatusError::ReadWorkspacesDir)?;
        let file_type = entry.file_type().map_err(StatusError::ReadWorkspacesDir)?;
        if !file_type.is_dir() {
            continue;
        }
        if entry.path().join("mission.md").is_file() {
            names.push(entry.file_name());
        }
    }
    names.sort_unstable_by(|a, b| b.cmp(a));
    Ok(names
        .into_iter()
        .map(|name| workspaces_dir.join(name))
        .collect())
}

struct RunningMission {
    mission_id: String,
    started_at_unix: i64,
    current_phase: String,
}

/// Ports `showRunningMissions` (`internal/cmd/status.go:124-188`).
fn show_running_missions(
    home: &Path,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), StatusError> {
    let events_dir = home.join("events");
    let entries = match std::fs::read_dir(&events_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            writeln!(
                error_output,
                "warning: could not scan running missions: {error}"
            )?;
            return Ok(());
        }
    };

    let mut running = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(mission_id) = file_name.strip_suffix(".jsonl") else {
            continue;
        };
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let Ok(Some(snap)) = project_mission_in_bytes(&bytes, mission_id) else {
            continue;
        };
        if snap.status != "in_progress" {
            continue;
        }
        let Some(started_at_unix) =
            super::time_fmt::parse_rfc3339_utc_to_unix_seconds(&snap.started_at)
        else {
            continue;
        };
        running.push(RunningMission {
            mission_id: mission_id.to_owned(),
            started_at_unix,
            current_phase: current_phase_name(&snap),
        });
    }

    if running.is_empty() {
        return Ok(());
    }
    running.sort_by_key(|mission| mission.started_at_unix);

    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);

    writeln!(output, "Running missions:")?;
    for mission in &running {
        let phase = if mission.current_phase.is_empty() {
            "(no active phase)"
        } else {
            mission.current_phase.as_str()
        };
        let elapsed = format_go_duration_seconds(now_unix - mission.started_at_unix);
        writeln!(
            output,
            "  {}  started {}  phase: {}  elapsed: {}",
            mission.mission_id,
            format_unix_seconds_ymd_hms(mission.started_at_unix),
            phase,
            elapsed,
        )?;
    }
    writeln!(output)?;
    Ok(())
}

/// Ports `currentPhaseName` (`internal/cmd/status.go:194-216`).
fn current_phase_name(snap: &MissionSnap) -> String {
    let mut latest: Option<(&PhaseSnap, i64)> = None;
    for phase in snap.phases.values() {
        if phase.status != "running" && phase.status != "retrying" {
            continue;
        }
        let Some(started) = super::time_fmt::parse_rfc3339_utc_to_unix_seconds(&phase.started_at)
        else {
            continue;
        };
        if latest.is_none_or(|(_, latest_started)| started > latest_started) {
            latest = Some((phase, started));
        }
    }
    if let Some((phase, _)) = latest {
        return phase.name.clone();
    }
    let mut latest_any: Option<(&PhaseSnap, i64)> = None;
    for phase in snap.phases.values() {
        let Some(started) = super::time_fmt::parse_rfc3339_utc_to_unix_seconds(&phase.started_at)
        else {
            continue;
        };
        if latest_any.is_none_or(|(_, latest_started)| started > latest_started) {
            latest_any = Some((phase, started));
        }
    }
    latest_any.map_or_else(String::new, |(phase, _)| phase.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn phase(name: &str, status: &str, started_at: &str) -> PhaseSnap {
        PhaseSnap {
            id: name.to_owned(),
            name: name.to_owned(),
            status: status.to_owned(),
            started_at: started_at.to_owned(),
            ended_at: String::new(),
        }
    }

    #[test]
    fn current_phase_name_prefers_running_over_completed() {
        let mut snap = MissionSnap {
            mission_id: "m1".to_owned(),
            status: "in_progress".to_owned(),
            started_at: "2026-07-13T00:00:00Z".to_owned(),
            ended_at: String::new(),
            phases: std::collections::BTreeMap::new(),
        };
        snap.phases.insert(
            "p1".to_owned(),
            phase("design", "completed", "2026-07-13T00:00:01Z"),
        );
        snap.phases.insert(
            "p2".to_owned(),
            phase("implement", "running", "2026-07-13T00:00:02Z"),
        );
        assert_eq!(current_phase_name(&snap), "implement");
    }

    #[test]
    fn current_phase_name_falls_back_to_latest_started() {
        let mut snap = MissionSnap {
            mission_id: "m1".to_owned(),
            status: "in_progress".to_owned(),
            started_at: "2026-07-13T00:00:00Z".to_owned(),
            ended_at: String::new(),
            phases: std::collections::BTreeMap::new(),
        };
        snap.phases.insert(
            "p1".to_owned(),
            phase("design", "completed", "2026-07-13T00:00:01Z"),
        );
        snap.phases.insert(
            "p2".to_owned(),
            phase("implement", "completed", "2026-07-13T00:00:05Z"),
        );
        assert_eq!(current_phase_name(&snap), "implement");
    }

    #[test]
    fn current_phase_name_empty_when_no_phases() {
        let snap = MissionSnap {
            mission_id: "m1".to_owned(),
            status: "in_progress".to_owned(),
            started_at: "2026-07-13T00:00:00Z".to_owned(),
            ended_at: String::new(),
            phases: std::collections::BTreeMap::new(),
        };
        assert_eq!(current_phase_name(&snap), "");
    }

    #[test]
    fn no_workspaces_found_prints_message() -> TestResult {
        let temp = std::env::temp_dir().join(format!("status-cmd-test-{}", std::process::id()));
        std::fs::create_dir_all(&temp)?;
        let mut output = Vec::new();
        let mut error_output = Vec::new();
        run(&temp, &mut output, &mut error_output)?;
        std::fs::remove_dir_all(&temp)?;
        assert_eq!(String::from_utf8(output)?, "no workspaces found\n");
        Ok(())
    }

    #[test]
    fn workspace_status_line_uses_checkpoint_fallback_when_no_event_log() -> TestResult {
        let temp =
            std::env::temp_dir().join(format!("status-cmd-checkpoint-test-{}", std::process::id()));
        let workspace = temp.join("workspaces").join("ws-1");
        std::fs::create_dir_all(&workspace)?;
        std::fs::write(workspace.join("mission.md"), "do the thing")?;
        std::fs::write(
            workspace.join("checkpoint.json"),
            r#"{"workspace_id":"ws-1","domain":"dev","status":"in_progress","plan":{"phases":[{"id":"p1","status":"completed"},{"id":"p2","status":"running"}]}}"#,
        )?;

        let mut output = Vec::new();
        let mut error_output = Vec::new();
        run(&temp, &mut output, &mut error_output)?;
        std::fs::remove_dir_all(&temp)?;

        let text = String::from_utf8(output)?;
        assert!(text.contains("ws-1 [in_progress] 1/2 phases"), "{text}");
        assert!(text.contains("do the thing"), "{text}");
        Ok(())
    }

    #[test]
    fn workspace_status_prefers_event_log_over_checkpoint() -> TestResult {
        let temp =
            std::env::temp_dir().join(format!("status-cmd-eventlog-test-{}", std::process::id()));
        let workspace = temp.join("workspaces").join("ws-2");
        std::fs::create_dir_all(&workspace)?;
        std::fs::create_dir_all(temp.join("events"))?;
        std::fs::write(workspace.join("mission.md"), "task")?;
        std::fs::write(
            workspace.join("checkpoint.json"),
            r#"{"workspace_id":"ws-2","domain":"dev","status":"in_progress","plan":{"phases":[{"id":"p1","status":"pending"}]}}"#,
        )?;
        std::fs::write(
            temp.join("events").join("ws-2.jsonl"),
            "{\"id\":\"evt_1\",\"type\":\"mission.started\",\"timestamp\":\"2026-07-13T00:00:00Z\",\"sequence\":1,\"mission_id\":\"ws-2\"}\n\
             {\"id\":\"evt_2\",\"type\":\"phase.started\",\"timestamp\":\"2026-07-13T00:00:01Z\",\"sequence\":2,\"mission_id\":\"ws-2\",\"phase_id\":\"p1\"}\n\
             {\"id\":\"evt_3\",\"type\":\"phase.completed\",\"timestamp\":\"2026-07-13T00:00:02Z\",\"sequence\":3,\"mission_id\":\"ws-2\",\"phase_id\":\"p1\"}\n\
             {\"id\":\"evt_4\",\"type\":\"mission.completed\",\"timestamp\":\"2026-07-13T00:00:03Z\",\"sequence\":4,\"mission_id\":\"ws-2\"}\n",
        )?;

        let mut output = Vec::new();
        let mut error_output = Vec::new();
        run(&temp, &mut output, &mut error_output)?;
        std::fs::remove_dir_all(&temp)?;

        let text = String::from_utf8(output)?;
        // Checkpoint says "pending"/0-of-1; the event log says "completed"/1.
        assert!(text.contains("ws-2 [completed] 1/1 phases"), "{text}");
        Ok(())
    }

    #[test]
    fn workspace_status_warns_and_falls_back_when_projection_fails() -> TestResult {
        // Ports `TestShowStatus_WarnsAndFallsBackWhenProjectionFails`
        // (status_test.go:55-84): an event-log read that fails projection
        // (here, an oversized line — see `project_mission_in_bytes`'s
        // `OversizedLine` error) must warn on stderr and fall back to the
        // checkpoint's own completed count, not abort the command.
        let temp =
            std::env::temp_dir().join(format!("status-cmd-warn-test-{}", std::process::id()));
        let workspace = temp.join("workspaces").join("ws-3");
        std::fs::create_dir_all(&workspace)?;
        std::fs::create_dir_all(temp.join("events"))?;
        std::fs::write(workspace.join("mission.md"), "task")?;
        std::fs::write(
            workspace.join("checkpoint.json"),
            r#"{"workspace_id":"ws-3","domain":"dev","status":"in_progress","plan":{"phases":[{"id":"p1","status":"completed"},{"id":"p2","status":"pending"}]}}"#,
        )?;
        // A single line larger than
        // `orchestrator_core::GO_EVENT_JSON_CONTENT_MAX_BYTES` (1 MiB - 1)
        // makes `project_mission_in_bytes` return `Err(OversizedLine)`.
        let oversized_line = format!(
            "{{\"pad\":\"{}\"}}\n",
            "x".repeat(orchestrator_core::GO_EVENT_JSON_CONTENT_MAX_BYTES + 1)
        );
        std::fs::write(temp.join("events").join("ws-3.jsonl"), oversized_line)?;

        let mut output = Vec::new();
        let mut error_output = Vec::new();
        run(&temp, &mut output, &mut error_output)?;
        std::fs::remove_dir_all(&temp)?;

        let out = String::from_utf8(output)?;
        assert!(out.contains("ws-3 [in_progress] 1/2 phases"), "{out}");
        let err = String::from_utf8(error_output)?;
        assert!(
            err.contains("warning: live status unavailable for ws-3:"),
            "{err}"
        );
        Ok(())
    }

    #[test]
    fn workspace_status_checkpoint_terminal_status_wins_over_live() -> TestResult {
        // Ports `TestShowStatus_CheckpointTerminalStatusWinsOverLive`
        // (status_test.go:171-207): once the checkpoint already carries a
        // terminal status (e.g. "failed"), a stale "in_progress" live
        // projection must not overwrite it — only `status.is_empty() ||
        // status == "in_progress"` triggers the live-status substitution
        // (status.rs:87).
        let temp =
            std::env::temp_dir().join(format!("status-cmd-cpwins-test-{}", std::process::id()));
        let workspace = temp.join("workspaces").join("ws-4");
        std::fs::create_dir_all(&workspace)?;
        std::fs::create_dir_all(temp.join("events"))?;
        std::fs::write(workspace.join("mission.md"), "task")?;
        std::fs::write(
            workspace.join("checkpoint.json"),
            r#"{"workspace_id":"ws-4","domain":"dev","status":"failed","plan":{"phases":[{"id":"p1","status":"completed"},{"id":"p2","status":"pending"}]}}"#,
        )?;
        // Stale live state that disagrees with the checkpoint terminal status.
        std::fs::write(
            temp.join("events").join("ws-4.jsonl"),
            "{\"id\":\"evt_1\",\"type\":\"mission.started\",\"timestamp\":\"2026-07-13T00:00:00Z\",\"sequence\":1,\"mission_id\":\"ws-4\"}\n\
             {\"id\":\"evt_2\",\"type\":\"phase.started\",\"timestamp\":\"2026-07-13T00:00:01Z\",\"sequence\":2,\"mission_id\":\"ws-4\",\"phase_id\":\"p1\"}\n",
        )?;

        let mut output = Vec::new();
        let mut error_output = Vec::new();
        run(&temp, &mut output, &mut error_output)?;
        std::fs::remove_dir_all(&temp)?;

        let out = String::from_utf8(output)?;
        assert!(out.contains("ws-4 [failed]"), "{out}");
        assert!(String::from_utf8(error_output)?.is_empty());
        Ok(())
    }

    #[test]
    fn running_missions_section_lists_in_progress_missions() -> TestResult {
        let temp =
            std::env::temp_dir().join(format!("status-cmd-running-test-{}", std::process::id()));
        std::fs::create_dir_all(temp.join("events"))?;
        std::fs::write(
            temp.join("events").join("m1.jsonl"),
            "{\"id\":\"evt_1\",\"type\":\"mission.started\",\"timestamp\":\"2026-07-13T00:00:00Z\",\"sequence\":1,\"mission_id\":\"m1\"}\n\
             {\"id\":\"evt_2\",\"type\":\"phase.started\",\"timestamp\":\"2026-07-13T00:00:01Z\",\"sequence\":2,\"mission_id\":\"m1\",\"phase_id\":\"p1\",\"data\":{\"name\":\"implement\"}}\n",
        )?;

        let mut output = Vec::new();
        let mut error_output = Vec::new();
        run(&temp, &mut output, &mut error_output)?;
        std::fs::remove_dir_all(&temp)?;

        let text = String::from_utf8(output)?;
        assert!(text.starts_with("Running missions:\n"), "{text}");
        assert!(text.contains("m1"), "{text}");
        assert!(text.contains("phase: implement"), "{text}");
        Ok(())
    }
}
