//! Tauri commands for notifications.
//!
//! Persists to `~/.alluka/whim/notifications.json` with 500-entry FIFO cap.
//! Watches scheduler state file and emits `whim://routines-changed` events.

use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{Emitter, State};

use crate::AppState;

// ── Constants ─────────────────────────────────────────────────────────────────

#[allow(dead_code)]
const NOTIFICATION_FIFO_CAP: usize = 500;

// ── Wire types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub title: String,
    pub message: String,
    pub timestamp: u64,
    pub read: bool,
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// List all notifications from `~/.alluka/whim/notifications.json`.
#[tauri::command]
pub fn list_notifications() -> Result<Vec<Notification>, String> {
    let notif_path = notifications_path()?;

    if !notif_path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(&notif_path)
        .map_err(|e| format!("failed to read notifications.json: {e}"))?;

    serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse notifications.json: {e}"))
}

/// Mark a notification as read/dismissed.
///
/// Emits `whim://notification-update` with the updated record so other
/// windows can replace their cached entry in place.
#[tauri::command]
pub fn notification_dismiss(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let mut notifications = list_notifications()?;
    let mut updated: Option<Notification> = None;
    if let Some(notif) = notifications.iter_mut().find(|n| n.id == id) {
        notif.read = true;
        updated = Some(notif.clone());
    }
    persist_notifications(&notifications)?;
    if let Some(notif) = updated {
        let _ = app.emit("whim://notification-update", &notif);
    }
    Ok(())
}

/// Mark all notifications as read/dismissed.
///
/// Emits `whim://notification-update` once per record that actually changed.
#[tauri::command]
pub fn notification_dismiss_all(app: tauri::AppHandle) -> Result<(), String> {
    let mut notifications = list_notifications()?;
    let mut updated: Vec<Notification> = Vec::new();
    for notif in &mut notifications {
        if !notif.read {
            notif.read = true;
            updated.push(notif.clone());
        }
    }
    persist_notifications(&notifications)?;
    for notif in &updated {
        let _ = app.emit("whim://notification-update", notif);
    }
    Ok(())
}

/// Start watching the scheduler state file and emit `whim://routines-changed` on updates.
#[tauri::command]
pub async fn start_routines_watcher(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let scheduler_state_path = dirs::home_dir()
        .ok_or("cannot determine home directory")?
        .join(".config/scheduler/scheduler.db");

    // If the path doesn't exist, we'll still set up the watcher and it will trigger
    // when the file is created or modified.
    let parent = scheduler_state_path
        .parent()
        .ok_or("invalid scheduler state path")?
        .to_path_buf();

    let (tx, mut rx) = tokio::sync::mpsc::channel(100);

    let watcher_result = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        match res {
            Ok(event) => {
                // Only trigger on file modifications
                if matches!(event.kind, notify::EventKind::Modify(_)) {
                    let _ = tx.blocking_send(());
                }
            }
            Err(e) => eprintln!("[whim] watcher error: {e}"),
        }
    });

    let mut watcher = watcher_result
        .map_err(|e| format!("failed to create watcher: {e}"))?;

    watcher
        .watch(&parent, RecursiveMode::NonRecursive)
        .map_err(|e| format!("failed to watch scheduler state: {e}"))?;

    // Store the watcher in app state
    *state
        .mission_watcher
        .lock()
        .map_err(|e| format!("failed to acquire watcher lock: {e}"))? = Some(watcher);

    // Spawn a task to forward events to the frontend
    let app = _app.clone();
    tokio::spawn(async move {
        while let Some(_) = rx.recv().await {
            eprintln!("[whim] scheduler state changed, emitting routines-changed");
            let _ = app.emit("whim://routines-changed", ());
        }
    });

    Ok(())
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Emit a notification and persist it to disk.
#[allow(dead_code)]
pub fn emit_notification(app: &tauri::AppHandle, title: String, message: String) -> Result<(), String> {
    let id = uuid::Uuid::new_v4().to_string();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("failed to get timestamp: {e}"))?
        .as_secs();

    let notif = Notification {
        id,
        title,
        message,
        timestamp,
        read: false,
    };

    // Emit to frontend
    let _ = app.emit("whim://notification", &notif);

    // Persist to disk
    let mut notifications = list_notifications()?;
    notifications.insert(0, notif);

    // Enforce FIFO cap
    if notifications.len() > NOTIFICATION_FIFO_CAP {
        notifications.truncate(NOTIFICATION_FIFO_CAP);
    }

    persist_notifications(&notifications)?;
    Ok(())
}

fn notifications_path() -> Result<PathBuf, String> {
    let whim_dir = dirs::home_dir()
        .ok_or("cannot determine home directory")?
        .join(".alluka/whim");

    Ok(whim_dir.join("notifications.json"))
}

fn persist_notifications(notifications: &[Notification]) -> Result<(), String> {
    let whim_dir = dirs::home_dir()
        .ok_or("cannot determine home directory")?
        .join(".alluka/whim");

    fs::create_dir_all(&whim_dir)
        .map_err(|e| format!("failed to create whim directory: {e}"))?;

    let notif_path = whim_dir.join("notifications.json");
    let content = serde_json::to_string_pretty(notifications)
        .map_err(|e| format!("failed to serialize notifications: {e}"))?;

    fs::write(&notif_path, content)
        .map_err(|e| format!("failed to write notifications.json: {e}"))
}
