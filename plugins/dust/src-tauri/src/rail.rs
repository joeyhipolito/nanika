//! Tauri commands for rail context: projects, routines, and pins.
//!
//! Data sources:
//!   - `~/.alluka/whim/active-context.json` — current project repo_root
//!   - `scheduler query items` — scheduled routines (JSON)
//!   - `~/.alluka/whim/pins.json` — pinned items (array)

use serde::{Deserialize, Serialize};
use std::fs;

// ── Wire types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub repo_root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routine {
    pub name: String,
    pub schedule: String,
    pub last_run: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinnedItem {
    pub id: String,
    pub title: String,
}

// ── Internal types ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ActiveContext {
    repo_root: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct SchedulerJob {
    name: String,
    schedule: String,
    last_run: Option<String>,
    #[serde(rename = "enabled")]
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct SchedulerResponse {
    items: Option<Vec<SchedulerJob>>,
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// List available projects. v0: single hard-coded project with repo_root
/// from `~/.alluka/whim/active-context.json` or fallback to default.
#[tauri::command]
pub fn list_projects() -> Result<Vec<Project>, String> {
    let repo_root = read_active_context()
        .unwrap_or_else(|_| "/Users/joeyhipolito/nanika".to_string());

    Ok(vec![Project {
        name: "nanika".to_string(),
        repo_root,
    }])
}

/// List scheduled routines by shelling out to `scheduler query items`.
#[tauri::command]
pub fn list_routines() -> Result<Vec<Routine>, String> {
    let output = std::process::Command::new("scheduler")
        .args(&["query", "items"])
        .output()
        .map_err(|e| format!("failed to run scheduler: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("scheduler command failed: {stderr}"));
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| format!("scheduler output not valid UTF-8: {e}"))?;

    let response: SchedulerResponse = serde_json::from_str(&stdout)
        .map_err(|e| format!("failed to parse scheduler JSON: {e}"))?;

    let routines = response
        .items
        .unwrap_or_default()
        .into_iter()
        .map(|job| Routine {
            name: job.name,
            schedule: job.schedule,
            last_run: job.last_run,
            status: if job.enabled {
                "enabled".to_string()
            } else {
                "disabled".to_string()
            },
        })
        .collect();

    Ok(routines)
}

/// Read pinned items from `~/.alluka/whim/pins.json`.
#[tauri::command]
pub fn read_pins() -> Result<Vec<PinnedItem>, String> {
    let pins_path = dirs::home_dir()
        .ok_or("cannot determine home directory")?
        .join(".alluka/whim/pins.json");

    if !pins_path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(&pins_path)
        .map_err(|e| format!("failed to read pins.json: {e}"))?;

    serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse pins.json: {e}"))
}

/// Write pinned items to `~/.alluka/whim/pins.json`.
#[tauri::command]
pub fn write_pins(pins: Vec<PinnedItem>) -> Result<(), String> {
    let whim_dir = dirs::home_dir()
        .ok_or("cannot determine home directory")?
        .join(".alluka/whim");

    fs::create_dir_all(&whim_dir)
        .map_err(|e| format!("failed to create whim directory: {e}"))?;

    let pins_path = whim_dir.join("pins.json");
    let content = serde_json::to_string_pretty(&pins)
        .map_err(|e| format!("failed to serialize pins: {e}"))?;

    fs::write(&pins_path, content)
        .map_err(|e| format!("failed to write pins.json: {e}"))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn read_active_context() -> Result<String, String> {
    let context_path = dirs::home_dir()
        .ok_or("cannot determine home directory")?
        .join(".alluka/whim/active-context.json");

    if !context_path.exists() {
        return Err("active-context.json not found".to_string());
    }

    let content = fs::read_to_string(&context_path)
        .map_err(|e| format!("failed to read active-context.json: {e}"))?;

    let ctx: ActiveContext = serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse active-context.json: {e}"))?;

    Ok(ctx.repo_root)
}
