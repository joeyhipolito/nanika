//! Tauri commands for reading orchestrator mission state and watching JSONL logs.
//!
//! Data is sourced from two canonical locations:
//!   - `~/.alluka/missions/*.md`         — mission files (YAML front-matter + markdown body)
//!   - `~/.alluka/workspaces/<id>/`       — checkpoint.json, worker output, log files
//!
//! The workspace ↔ mission link is the `mission_path` file in each workspace dir.

use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

use crate::AppState;

// ── Wire types sent to the React frontend ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionSummary {
    pub id: String,
    pub slug: String,
    pub status: String,
    pub started_at: String,
    pub last_event_at: Option<String>,
    pub phase_count: u32,
    pub phases_done: u32,
    pub phases_failed: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseSummary {
    pub phase: String,
    pub status: String,
    pub persona: String,
    pub skills: Vec<String>,
    pub depends: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionDetail {
    #[serde(flatten)]
    pub summary: MissionSummary,
    pub phases: Vec<PhaseSummary>,
    pub workspace_root: String,
    pub repo_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseDetail {
    pub phase: String,
    pub status: String,
    pub persona: String,
    pub skills: Vec<String>,
    pub depends: Vec<String>,
    pub output_files: Vec<String>,
    pub worker_log_tail: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum GateDecision {
    Approve,
    Reject,
}

// ── Internal checkpoint.json shapes ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CheckpointFile {
    payload: CheckpointPayload,
}

#[derive(Debug, Deserialize)]
struct CheckpointPayload {
    status: Option<String>,
    started_at: Option<String>,
    git_repo_root: Option<String>,
    plan: Option<CheckpointPlan>,
}

#[derive(Debug, Deserialize)]
struct CheckpointPlan {
    phases: Vec<CheckpointPhase>,
}

#[derive(Debug, Deserialize)]
struct CheckpointPhase {
    id: Option<String>,
    name: Option<String>,
    status: Option<String>,
    persona: Option<String>,
    skills: Option<serde_json::Value>,
    dependencies: Option<Vec<String>>,
}

// ── Path helpers ──────────────────────────────────────────────────────────────

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

fn missions_dir() -> PathBuf {
    home_dir().join(".alluka").join("missions")
}

fn workspaces_dir() -> PathBuf {
    home_dir().join(".alluka").join("workspaces")
}

// ── Epoch → RFC3339 (mirrors lib.rs to_rfc3339, duplicated to stay in module) ─

fn epoch_to_rfc3339(epoch: i64) -> String {
    let days = epoch / 86400;
    let tod = epoch - days * 86400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let h = tod / 3600;
    let mi = (tod % 3600) / 60;
    let s = tod % 60;
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn mtime_rfc3339(path: &Path) -> Option<String> {
    use std::time::UNIX_EPOCH;
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = mtime.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    Some(epoch_to_rfc3339(secs))
}

// ── Workspace ↔ mission linking ───────────────────────────────────────────────

/// Builds a map from mission_file_path (absolute string) → workspace_id.
/// Each workspace contains a `mission_path` file with the absolute path to its mission file.
fn build_workspace_map() -> HashMap<String, String> {
    let ws_dir = workspaces_dir();
    let mut map: HashMap<String, String> = HashMap::new();
    let Ok(entries) = std::fs::read_dir(&ws_dir) else {
        return map;
    };
    for entry in entries.flatten() {
        let ws_path = entry.path();
        if !ws_path.is_dir() {
            continue;
        }
        let ws_id = entry.file_name().to_string_lossy().to_string();
        if let Ok(mission_path) = std::fs::read_to_string(ws_path.join("mission_path")) {
            map.insert(mission_path.trim().to_string(), ws_id);
        }
    }
    map
}

// ── Checkpoint helpers ────────────────────────────────────────────────────────

fn read_checkpoint(workspace_id: &str) -> Option<CheckpointFile> {
    let path = workspaces_dir().join(workspace_id).join("checkpoint.json");
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn checkpoint_phases<'a>(cp: &'a CheckpointFile) -> &'a [CheckpointPhase] {
    cp.payload.plan.as_ref().map(|p| p.phases.as_slice()).unwrap_or(&[])
}

fn phase_skills(cp_phase: &CheckpointPhase) -> Vec<String> {
    match &cp_phase.skills {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

fn build_mission_summary(mission_file: &Path, workspace_id: &str, cp: &CheckpointFile) -> MissionSummary {
    let slug = mission_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let phases = checkpoint_phases(cp);
    let phase_count = phases.len() as u32;
    let phases_done = phases.iter().filter(|p| p.status.as_deref() == Some("completed")).count() as u32;
    let phases_failed = phases.iter().filter(|p| p.status.as_deref() == Some("failed")).count() as u32;

    let status = cp.payload.status.clone().unwrap_or_else(|| "unknown".to_string());
    let started_at = cp.payload.started_at.clone().unwrap_or_default();
    let last_event_at = mtime_rfc3339(&workspaces_dir().join(workspace_id).join("checkpoint.json"));

    MissionSummary {
        id: workspace_id.to_string(),
        slug,
        status,
        started_at,
        last_event_at,
        phase_count,
        phases_done,
        phases_failed,
    }
}

// ── Log-file predicate ────────────────────────────────────────────────────────

fn is_log_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.ends_with(".log") || name == "output.md"
}

// ── Commands ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_missions() -> Result<Vec<MissionSummary>, String> {
    tokio::task::spawn_blocking(move || {
        let ws_map = build_workspace_map();
        let Ok(entries) = std::fs::read_dir(missions_dir()) else {
            return Ok(Vec::new());
        };

        let mut results: Vec<MissionSummary> = entries
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
            .filter_map(|e| {
                let path = e.path();
                let path_str = path.to_string_lossy().to_string();
                let ws_id = ws_map.get(&path_str)?;
                let cp = read_checkpoint(ws_id)?;
                Some(build_mission_summary(&path, ws_id, &cp))
            })
            .collect();

        results.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(results)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn get_mission(mission_id: String) -> Result<MissionDetail, String> {
    tokio::task::spawn_blocking(move || {
        let ws_dir = workspaces_dir().join(&mission_id);

        let mission_path_raw = std::fs::read_to_string(ws_dir.join("mission_path"))
            .map_err(|e| format!("read mission_path: {e}"))?;
        let mission_file = PathBuf::from(mission_path_raw.trim());

        let cp = read_checkpoint(&mission_id).ok_or("checkpoint not found")?;
        let summary = build_mission_summary(&mission_file, &mission_id, &cp);

        let phases: Vec<PhaseSummary> = checkpoint_phases(&cp)
            .iter()
            .map(|p| PhaseSummary {
                phase: p.name.clone().unwrap_or_default(),
                status: p.status.clone().unwrap_or_else(|| "pending".to_string()),
                persona: p.persona.clone().unwrap_or_default(),
                skills: phase_skills(p),
                depends: p.dependencies.clone().unwrap_or_default(),
            })
            .collect();

        Ok(MissionDetail {
            summary,
            phases,
            workspace_root: ws_dir.to_string_lossy().to_string(),
            repo_root: cp.payload.git_repo_root.clone(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn get_phase(mission_id: String, phase_id: String) -> Result<PhaseDetail, String> {
    tokio::task::spawn_blocking(move || {
        let ws_dir = workspaces_dir().join(&mission_id);
        let cp = read_checkpoint(&mission_id).ok_or("checkpoint not found")?;
        let phases = checkpoint_phases(&cp);

        let (phase_idx, phase) = phases
            .iter()
            .enumerate()
            .find(|(_, p)| {
                p.id.as_deref() == Some(&phase_id) || p.name.as_deref() == Some(&phase_id)
            })
            .ok_or_else(|| format!("phase {phase_id} not found"))?;

        let persona = phase.persona.clone().unwrap_or_default();
        // Worker dirs are named <persona>-phase-<1-based-index>
        let worker_dir = ws_dir.join("workers").join(format!("{persona}-phase-{}", phase_idx + 1));

        let mut output_files: Vec<String> = Vec::new();
        let mut log_paths: Vec<PathBuf> = Vec::new();

        if worker_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&worker_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if is_log_file(&p) {
                        log_paths.push(p.clone());
                    }
                    output_files.push(p.to_string_lossy().to_string());
                }
            }
        }

        // Collect last 100 lines across all log files
        let mut tail: Vec<String> = Vec::new();
        for log_path in &log_paths {
            if tail.len() >= 100 {
                break;
            }
            if let Ok(content) = std::fs::read_to_string(log_path) {
                let lines: Vec<&str> = content.lines().collect();
                let want = (100 - tail.len()).min(lines.len());
                let start = lines.len() - want;
                tail.extend(lines[start..].iter().map(|s| s.to_string()));
            }
        }

        Ok(PhaseDetail {
            phase: phase.name.clone().unwrap_or_default(),
            status: phase.status.clone().unwrap_or_else(|| "pending".to_string()),
            persona,
            skills: phase_skills(phase),
            depends: phase.dependencies.clone().unwrap_or_default(),
            output_files,
            worker_log_tail: tail,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn mission_approve_gate(
    mission_id: String,
    gate_id: String,
    decision: GateDecision,
) -> Result<(), String> {
    let decision_str = match decision {
        GateDecision::Approve => "approve",
        GateDecision::Reject => "reject",
    };
    let out = tokio::process::Command::new("orchestrator")
        .args(["gate", &mission_id, &gate_id, decision_str])
        .output()
        .await
        .map_err(|e| format!("spawn orchestrator gate: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "orchestrator gate failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn mission_cancel(mission_id: String) -> Result<(), String> {
    let out = tokio::process::Command::new("orchestrator")
        .args(["cancel", &mission_id])
        .output()
        .await
        .map_err(|e| format!("spawn orchestrator cancel: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "orchestrator cancel failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn phase_rerun(mission_id: String, phase_id: String) -> Result<(), String> {
    eprintln!("[dust] phase_rerun mission={mission_id} phase={phase_id}: not yet supported by orchestrator CLI");
    Err("phase_rerun not yet supported by orchestrator CLI".to_string())
}

/// Start a file watcher on `~/.alluka/workspaces/<mission_id>/`.
///
/// - `checkpoint.json` changes → emit `whim://mission-event` with parsed checkpoint
/// - `*.log` / `output.md` changes → tail new lines from last-known offset →
///   emit `whim://mission-run-output` per new line
///
/// The watcher handle is stored in `AppState.mission_watcher`; calling this
/// again atomically replaces the previous watcher (old thread exits when its
/// channel disconnects).
#[tauri::command]
pub async fn start_mission_run_watcher(
    mission_id: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    let ws_dir = workspaces_dir().join(&mission_id);
    if !ws_dir.exists() {
        return Err(format!("workspace {mission_id} not found"));
    }

    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let _ = tx.send(res);
    })
    .map_err(|e| e.to_string())?;

    watcher
        .watch(&ws_dir, RecursiveMode::Recursive)
        .map_err(|e| e.to_string())?;

    // Atomically replace the old watcher (dropping it disconnects its channel → thread exits).
    *state.mission_watcher.lock().map_err(|e| e.to_string())? = Some(watcher);

    // Debounce + dispatch thread — entirely self-contained, no shared AppState needed.
    let mission_id_thread = mission_id.clone();
    std::thread::spawn(move || {
        const DEBOUNCE: Duration = Duration::from_millis(100);
        const POLL: Duration = Duration::from_millis(25);

        let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
        // Per-file read offsets so we emit only new lines (tail behaviour).
        let mut offsets: HashMap<PathBuf, u64> = HashMap::new();

        loop {
            match rx.recv_timeout(POLL) {
                Ok(Ok(event)) => {
                    let is_relevant = matches!(
                        event.kind,
                        notify::EventKind::Create(_) | notify::EventKind::Modify(_)
                    );
                    if is_relevant {
                        for p in event.paths {
                            pending.insert(p, Instant::now());
                        }
                    }
                }
                Ok(Err(_)) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }

            let now = Instant::now();
            let ready: Vec<PathBuf> = pending
                .iter()
                .filter(|(_, t)| now.duration_since(**t) >= DEBOUNCE)
                .map(|(p, _)| p.clone())
                .collect();

            for path in ready {
                pending.remove(&path);
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

                if name == "checkpoint.json" {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                            let _ = app.emit(
                                "whim://mission-event",
                                serde_json::json!({ "mission_id": mission_id_thread, "checkpoint": val }),
                            );
                        }
                    }
                } else if is_log_file(&path) {
                    let offset = offsets.entry(path.clone()).or_insert(0);
                    if let Ok(mut f) = std::fs::File::open(&path) {
                        use std::io::{Read, Seek, SeekFrom};
                        if f.seek(SeekFrom::Start(*offset)).is_ok() {
                            let mut buf = String::new();
                            if f.read_to_string(&mut buf).is_ok() {
                                *offset += buf.len() as u64;
                                for line in buf.lines() {
                                    let _ = app.emit(
                                        "whim://mission-run-output",
                                        serde_json::json!({
                                            "mission_id": mission_id_thread,
                                            "file": path.to_string_lossy(),
                                            "line": line,
                                        }),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    });

    Ok(())
}
