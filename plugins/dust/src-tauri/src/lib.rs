//! Tauri 2 host for the dust desktop app.
//!
//! Bridges dust-registry to Tauri commands consumed by the React frontend.
//!
//! - `search_capabilities` — fuzzy-search, returns Vec<CapabilityMatch>
//! - `get_plugin_info`     — manifest + liveness for one plugin
//! - `render_ui`           — first Component from registry render
//! - `dispatch_action`     — execute a plugin action

use dust_core::envelope::EventType;
use dust_core::Component;
use dust_registry::{ConnectionId, EventStream, Registry};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{Emitter, Manager, State};
use tokio::sync::Mutex as TokioMutex;

// ---------------------------------------------------------------------------
// Wire types — serialised to JSON for the React frontend
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityMatch {
    pub plugin_id: String,
    pub plugin_name: String,
    pub capability: CapabilityInfo,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfoManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub capabilities: Vec<CapabilityInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub manifest: PluginInfoManifest,
    pub healthy: bool,
}

// ---------------------------------------------------------------------------
// Chat subscription types
// ---------------------------------------------------------------------------

/// Payload emitted on the `dust://chat-event` Tauri channel.
#[derive(Debug, Clone, Serialize)]
pub struct ChatEventPayload {
    pub thread_id: Option<String>,
    pub event_type: String,
    pub data: serde_json::Value,
}

/// Active chat subscription held in `AppState`.
///
/// Consecutive `chat_subscribe` calls atomically replace this slot: a new
/// `EventStream` is opened *before* the old forward task is aborted, so no
/// delta window exists between the two subscriptions.
pub struct ChatSubscription {
    pub thread_id: Option<String>,
    pub conn_id: ConnectionId,
    pub subscription_id: String,
    pub forward_task: tokio::task::JoinHandle<()>,
}

// ---------------------------------------------------------------------------
// Managed state
// ---------------------------------------------------------------------------

pub struct AppState {
    pub registry: Arc<Registry>,
    /// At most one live chat subscription per webview.  `Mutex` because
    /// `chat_subscribe` mutates this across an async boundary.
    pub chat_sub: TokioMutex<Option<ChatSubscription>>,
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
async fn search_capabilities(
    query: String,
    state: State<'_, AppState>,
) -> Result<Vec<CapabilityMatch>, String> {
    let results = state.registry.search_with_ids(&query).await;
    let mut matches = Vec::new();
    for (plugin_id, manifest) in results {
        for cap in &manifest.capabilities {
            let (cap_id, cap_name, cap_desc, keywords) = capability_fields(cap);
            matches.push(CapabilityMatch {
                plugin_id: plugin_id.clone(),
                plugin_name: manifest.name.clone(),
                capability: CapabilityInfo { id: cap_id, name: cap_name, description: cap_desc, keywords },
                score: 1.0,
            });
        }
    }
    Ok(matches)
}

#[tauri::command]
async fn get_plugin_info(
    plugin_id: String,
    state: State<'_, AppState>,
) -> Result<PluginInfo, String> {
    let results = state.registry.search_with_ids("").await;
    let (_, manifest) = results
        .into_iter()
        .find(|(id, _)| *id == plugin_id)
        .ok_or_else(|| format!("plugin not found: {plugin_id}"))?;

    let capabilities = manifest.capabilities.iter().map(|c| {
        let (id, name, desc, keywords) = capability_fields(c);
        CapabilityInfo { id, name, description: desc, keywords }
    }).collect();

    Ok(PluginInfo {
        manifest: PluginInfoManifest {
            id: plugin_id,
            name: manifest.name,
            version: manifest.version,
            description: manifest.description,
            capabilities,
        },
        healthy: true,
    })
}

#[tauri::command]
async fn render_ui(
    plugin_id: String,
    _capability_id: String,
    _query: String,
    state: State<'_, AppState>,
) -> Result<Vec<Component>, String> {
    state
        .registry
        .render_ui(&plugin_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn dispatch_action(
    plugin_id: String,
    _capability_id: String,
    action_id: String,
    params: serde_json::Value,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    eprintln!("[dust] dispatch_action: plugin={plugin_id} action={action_id} params={params}");
    use std::collections::HashMap;
    // Frontend sends `{ id?: string, ...args }`. Pull `id` out to
    // `item_id` so tracker-style dispatches (`item_id=TRK-42`) still work;
    // the rest of the keys become `args`.
    let mut args: HashMap<String, serde_json::Value> = match params {
        serde_json::Value::Object(m) => m.into_iter().collect(),
        serde_json::Value::Null => HashMap::new(),
        other => {
            let mut m = HashMap::new();
            m.insert("value".to_string(), other);
            m
        }
    };
    let item_id = args
        .remove("id")
        .and_then(|v| v.as_str().map(|s| s.to_owned()));
    let ap = dust_core::envelope::ActionParams {
        op_id: Some(action_id),
        item_id,
        args,
    };
    // Return the full ActionResult ({success, message?, data?}) so callers can
    // read response payloads (list_threads, list_messages, new_thread). Older
    // call sites that ignore the return value are unaffected.
    state
        .registry
        .dispatch_action(&plugin_id, ap)
        .await
        .and_then(|r| {
            serde_json::to_value(&r).map_err(|e| {
                dust_registry::RegistryError::Ipc(format!("serialize action result: {e}"))
            })
        })
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Chat subscription commands
// ---------------------------------------------------------------------------

/// Subscribe to live chat events from the `chat` plugin.
///
/// Atomically replaces any existing subscription: the new broadcast receiver
/// is obtained *before* the old forward task is aborted (zero-loss replace).
/// Consecutive calls without an intervening `chat_unsubscribe` result in
/// exactly one active forwarder task — the old task is aborted and its
/// registry slot released after the new subscription is in place.
///
/// Events are forwarded to the frontend as `dust://chat-event` payloads.
/// Only `data_updated` and `error` envelopes are forwarded; all others are
/// dropped at the forward task boundary.
#[tauri::command]
async fn chat_subscribe(
    thread_id: Option<String>,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    eprintln!("[dust] chat_subscribe: thread_id={thread_id:?}");
    // Hold the lock for the entire replacement — prevents a concurrent
    // chat_subscribe from racing the new-slot allocation.
    let mut guard = state.chat_sub.lock().await;

    // Open the new stream *before* touching the old one (zero-loss replace).
    let stream: EventStream = state
        .registry
        .open_event_stream("chat")
        .await
        .map_err(|e| e.to_string())?;

    let conn_id = stream.conn_id;
    let subscription_id = stream.subscription_id.clone();
    let thread_id_fallback = thread_id.clone();

    let forward_task = tokio::spawn(async move {
        let mut s = stream;
        eprintln!("[dust] forward_task STARTED");
        loop {
            let Some(event) = s.next().await else { eprintln!("[dust] forward_task: stream ended"); break };
            eprintln!("[dust] forward_task: got event type={:?}", event.event_type);
            let type_str = match event.event_type {
                EventType::DataUpdated => "data_updated",
                EventType::Error => "error",
                _ => { eprintln!("[dust] forward_task: skipping non-data/error event"); continue },
            };
            // Prefer the thread_id embedded in event data (server-assigned) over
            // the value captured at subscribe time — critical for ⌘N where the
            // client subscribes with null and the server assigns a new thread_id.
            let thread_id_out = event
                .data
                .as_object()
                .and_then(|o| o.get("thread_id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned())
                .or_else(|| thread_id_fallback.clone());
            let payload = ChatEventPayload {
                thread_id: thread_id_out,
                event_type: type_str.to_string(),
                data: event.data,
            };
            let _ = app.emit("dust://chat-event", payload);
        }
    });

    let new_sub = ChatSubscription {
        thread_id,
        conn_id,
        subscription_id,
        forward_task,
    };

    let old = guard.replace(new_sub);
    drop(guard);

    // Tear down the old subscription after releasing the lock so `disconnect_subscriber`
    // does not block a concurrent subscribe.
    if let Some(sub) = old {
        sub.forward_task.abort();
        let _ = state
            .registry
            .disconnect_subscriber("chat", sub.conn_id, &sub.subscription_id)
            .await;
    }

    Ok(())
}

/// Cancel the active chat subscription, if any.
///
/// Idempotent — safe to call when no subscription is active.
#[tauri::command]
async fn chat_unsubscribe(state: State<'_, AppState>) -> Result<(), String> {
    let old = state.chat_sub.lock().await.take();
    if let Some(sub) = old {
        sub.forward_task.abort();
        let _ = state
            .registry
            .disconnect_subscriber("chat", sub.conn_id, &sub.subscription_id)
            .await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// File I/O commands
// ---------------------------------------------------------------------------

/// Canonicalize `raw` and reject anything that escapes `$HOME`.
///
/// For files that don't exist yet (QuickEdit save path), we canonicalize the
/// parent directory and rejoin the basename — `std::fs::canonicalize` fails
/// on non-existent paths.
fn validate_path(raw: &str) -> Result<std::path::PathBuf, String> {
    use std::path::PathBuf;
    let p = PathBuf::from(raw);
    let canonical = match std::fs::canonicalize(&p) {
        Ok(c) => c,
        Err(_) => {
            let parent = p
                .parent()
                .ok_or_else(|| "path has no parent".to_string())?;
            let basename = p
                .file_name()
                .ok_or_else(|| "path has no filename".to_string())?;
            std::fs::canonicalize(parent)
                .map_err(|e| format!("canonicalize parent: {e}"))?
                .join(basename)
        }
    };
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    let home_canonical =
        std::fs::canonicalize(&home).map_err(|e| format!("canonicalize HOME: {e}"))?;
    if !canonical.starts_with(&home_canonical) {
        return Err(format!(
            "path outside allowed root (HOME): {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

#[tauri::command]
async fn read_file(path: String) -> Result<String, String> {
    let safe = validate_path(&path)?;
    std::fs::read_to_string(&safe).map_err(|e| e.to_string())
}

#[tauri::command]
async fn write_file(
    path: String,
    content: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    use std::fs;
    let safe = validate_path(&path)?;
    let tmp = safe.with_extension(format!(
        "{}.tmp",
        safe.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    fs::write(&tmp, &content).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &safe).map_err(|e| e.to_string())?;
    app.emit(
        "file_changed",
        serde_json::json!({ "path": safe.to_string_lossy() }),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ── FileRef support commands ─────────────────────────────────────────────────

/// Lightweight file metadata returned to the FileRef popover.
#[derive(serde::Serialize)]
struct FileStat {
    size: u64,
    mtime_rfc3339: String,
    /// Extension-derived language hint, or `None` for unknown extensions.
    language: Option<String>,
}

fn guess_language(path: &std::path::Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    let name = match ext.as_str() {
        "rs" => "rust",
        "py" => "python",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "go" => "go",
        "sh" | "bash" | "zsh" => "shell",
        "md" | "markdown" => "markdown",
        "json" => "json",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "html" | "htm" => "html",
        "css" => "css",
        "c" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "h" => "cpp",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        _ => return None,
    };
    Some(name.to_string())
}

fn to_rfc3339(t: std::time::SystemTime) -> String {
    use std::time::UNIX_EPOCH;
    let dur = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let epoch = dur.as_secs() as i64;
    // civil_from_days from the same family as tracker/scheduler helpers.
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

#[tauri::command]
async fn stat_file(path: String) -> Result<FileStat, String> {
    let safe = validate_path(&path)?;
    let meta = std::fs::metadata(&safe).map_err(|e| e.to_string())?;
    let mtime = meta.modified().map_err(|e| e.to_string())?;
    Ok(FileStat {
        size: meta.len(),
        mtime_rfc3339: to_rfc3339(mtime),
        language: guess_language(&safe),
    })
}

/// One line of preview content, with its 1-based line number.
#[derive(serde::Serialize)]
struct PreviewLine {
    number: u32,
    text: String,
}

#[derive(serde::Serialize)]
struct PreviewSlice {
    lines: Vec<PreviewLine>,
    truncated: bool,
}

#[tauri::command]
async fn preview_file_slice(
    path: String,
    line: Option<u32>,
    context: Option<u32>,
) -> Result<PreviewSlice, String> {
    let safe = validate_path(&path)?;
    let content = std::fs::read_to_string(&safe).map_err(|e| e.to_string())?;
    let ctx = context.unwrap_or(10).min(200);
    let all: Vec<&str> = content.lines().collect();
    let total = all.len() as u32;
    if total == 0 {
        return Ok(PreviewSlice { lines: Vec::new(), truncated: false });
    }
    // 1-based line. If None, return the first 2*context+1 lines (file top).
    let center = line.unwrap_or(1).max(1).min(total);
    let start = center.saturating_sub(ctx).max(1);
    let end = (center + ctx).min(total);
    let mut lines = Vec::with_capacity((end - start + 1) as usize);
    for n in start..=end {
        let idx = (n - 1) as usize;
        lines.push(PreviewLine {
            number: n,
            text: all[idx].to_string(),
        });
    }
    Ok(PreviewSlice {
        lines,
        truncated: start > 1 || end < total,
    })
}

/// Pick an editor command from the environment, preferring $EDITOR then
/// falling back to `code` then `vi`. Returns the argv[0] token + a cursor
/// helper closure that appends whatever flag the editor uses for `line`.
fn resolve_editor() -> (String, &'static str) {
    if let Ok(ed) = std::env::var("EDITOR") {
        if !ed.trim().is_empty() {
            let kind = if ed.contains("code") {
                "code"
            } else if ed.contains("vi") || ed.contains("nvim") || ed.contains("nano") {
                "plus"
            } else {
                "plain"
            };
            return (ed, kind);
        }
    }
    if which::which("code").is_ok() {
        return ("code".to_string(), "code");
    }
    ("vi".to_string(), "plus")
}

#[tauri::command]
async fn open_in_editor(path: String, line: Option<u32>) -> Result<(), String> {
    let safe = validate_path(&path)?;
    let (raw_cmd, kind) = resolve_editor();
    // Split the EDITOR value into the binary + any pre-existing args (e.g.
    // users set EDITOR="code --wait"). We shell-word split manually.
    let mut parts: Vec<String> = raw_cmd.split_whitespace().map(String::from).collect();
    let bin = parts.remove(0);
    let path_str = safe.to_string_lossy().to_string();
    match (kind, line) {
        ("code", Some(n)) => {
            parts.push("--goto".to_string());
            parts.push(format!("{}:{}", path_str, n));
        }
        ("plus", Some(n)) => {
            parts.push(format!("+{}", n));
            parts.push(path_str);
        }
        _ => {
            parts.push(path_str);
        }
    }
    std::process::Command::new(&bin)
        .args(&parts)
        .spawn()
        .map_err(|e| format!("spawn {bin}: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn capability_fields(cap: &dust_core::Capability) -> (String, String, String, Vec<String>) {
    match cap {
        dust_core::Capability::Command { prefix } => (
            format!("cmd:{prefix}"),
            format!("Command: {prefix}"),
            format!("Invoke '{prefix}' commands"),
            vec![prefix.clone(), "command".into()],
        ),
        dust_core::Capability::Widget { refresh_secs } => (
            "widget".into(),
            "Widget".into(),
            if *refresh_secs > 0 { format!("Auto-refreshes every {refresh_secs}s") } else { "Static widget".into() },
            vec!["widget".into()],
        ),
        dust_core::Capability::Scheduler => (
            "scheduler".into(),
            "Scheduler".into(),
            "Handles background scheduled jobs".into(),
            vec!["scheduler".into(), "schedule".into()],
        ),
    }
}

// ---------------------------------------------------------------------------
// Window mode command — resize + reposition for default/expanded/collapsed
// ---------------------------------------------------------------------------

#[tauri::command]
async fn set_window_mode(
    mode: String,
    handle: tauri::AppHandle,
) -> Result<(), String> {
    let window = handle.get_webview_window("main").ok_or("no window")?;
    let monitors = window.available_monitors().unwrap_or_default();

    // Find the monitor the cursor is on; fall back to index 0.
    let monitor_idx: Option<usize> = if monitors.is_empty() {
        None
    } else if let Ok(cursor) = handle.cursor_position() {
        let found = monitors.iter().position(|m| {
            let px = m.position().x as f64;
            let py = m.position().y as f64;
            let sw = m.size().width as f64;
            let sh = m.size().height as f64;
            cursor.x >= px && cursor.x < px + sw && cursor.y >= py && cursor.y < py + sh
        });
        Some(found.unwrap_or(0))
    } else {
        Some(0)
    };

    // Use LogicalSize/Position so the math is DPI-independent. Monitor
    // methods return physical units, so divide by scale_factor to convert.
    let (w_log, h_log) = match mode.as_str() {
        "default" => (820.0_f64, 520.0_f64),
        "expanded" => (1200.0_f64, 800.0_f64),
        "collapsed" => (0.0, 52.0), // width recomputed below from monitor
        _ => return Err(format!("unknown mode: {mode}")),
    };

    if mode == "collapsed" {
        // macOS menu bar is ~25pt logical (more on notched Macs).
        let menu_bar_logical = 25.0_f64;
        if let Some(idx) = monitor_idx {
            let m = &monitors[idx];
            let scale = m.scale_factor();
            let mon_w_logical = m.size().width as f64 / scale;
            let mon_x_logical = m.position().x as f64 / scale;
            let mon_y_logical = m.position().y as f64 / scale;
            window.set_size(tauri::LogicalSize::new(mon_w_logical, h_log))
                .map_err(|e| e.to_string())?;
            window
                .set_position(tauri::LogicalPosition::new(
                    mon_x_logical,
                    mon_y_logical + menu_bar_logical,
                ))
                .map_err(|e| e.to_string())?;
        } else {
            window.set_size(tauri::LogicalSize::new(1440.0, h_log))
                .map_err(|e| e.to_string())?;
            window
                .set_position(tauri::LogicalPosition::new(0.0, menu_bar_logical))
                .map_err(|e| e.to_string())?;
        }
    } else {
        window.set_size(tauri::LogicalSize::new(w_log, h_log))
            .map_err(|e| e.to_string())?;
        if let Some(idx) = monitor_idx {
            let m = &monitors[idx];
            let scale = m.scale_factor();
            let mon_w_logical = m.size().width as f64 / scale;
            let mon_h_logical = m.size().height as f64 / scale;
            let mon_x_logical = m.position().x as f64 / scale;
            let mon_y_logical = m.position().y as f64 / scale;
            let x = mon_x_logical + (mon_w_logical - w_log) / 2.0;
            let y = mon_y_logical + (mon_h_logical - h_log) / 2.0;
            window.set_position(tauri::LogicalPosition::new(x, y))
                .map_err(|e| e.to_string())?;
        } else {
            window.center().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Window positioning — center on the monitor under the cursor
// ---------------------------------------------------------------------------

fn center_on_active_monitor(handle: &tauri::AppHandle, window: &tauri::WebviewWindow) {
    let monitors = match window.available_monitors() {
        Ok(m) => m,
        Err(_) => { let _ = window.center(); return; }
    };
    if monitors.is_empty() { let _ = window.center(); return; }

    let target = if let Ok(cursor) = handle.cursor_position() {
        monitors.iter().find(|m| {
            let px = m.position().x as f64;
            let py = m.position().y as f64;
            let sw = m.size().width as f64;
            let sh = m.size().height as f64;
            cursor.x >= px && cursor.x < px + sw && cursor.y >= py && cursor.y < py + sh
        }).unwrap_or(&monitors[0])
    } else {
        &monitors[0]
    };

    let scale = target.scale_factor();
    let mon_w_logical = target.size().width as f64 / scale;
    let mon_h_logical = target.size().height as f64 / scale;
    let mon_x_logical = target.position().x as f64 / scale;
    let mon_y_logical = target.position().y as f64 / scale;
    let x = mon_x_logical + (mon_w_logical - 820.0) / 2.0;
    let y = mon_y_logical + (mon_h_logical - 520.0) / 2.0;
    let _ = window.set_position(tauri::LogicalPosition::new(x, y));
}

// ---------------------------------------------------------------------------
// App entry point
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let registry = tauri::async_runtime::block_on(async {
        Registry::new().await.unwrap_or_else(|e| {
            eprintln!("[dust] registry init failed: {e}");
            panic!("cannot start without registry: {e}");
        })
    });

    tauri::Builder::default()
        .setup(|app| {
            use tauri_plugin_global_shortcut::{
                Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState,
            };

            // macOS: hide from Dock and Cmd+Tab switcher — dust is summoned by hotkey only
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Center the pane on the active monitor at startup. The
            // tauri.conf.json "center": true flag only centers on the
            // primary monitor; this re-centers on whichever monitor
            // currently holds the cursor (matches hotkey-summon behavior).
            if let Some(window) = app.get_webview_window("main") {
                center_on_active_monitor(&app.handle(), &window);
            }

            let handle = app.handle().clone();
            app.handle().plugin(
                tauri_plugin_global_shortcut::Builder::new()
                    .with_handler(move |_app, _shortcut, event| {
                        if event.state() != ShortcutState::Pressed { return; }
                        let Some(window) = handle.get_webview_window("main") else { return; };
                        let visible = window.is_visible().unwrap_or(false);
                        if visible {
                            // Let the frontend animate out before the OS hides the window
                            let _ = window.emit("dust://hide-request", ());
                        } else {
                            center_on_active_monitor(&handle, &window);
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    })
                    .build(),
            )?;

            // ⌥Space; fall back to ⌥⌘Space if the shortcut is already claimed
            let alt_space = Shortcut::new(Some(Modifiers::ALT), Code::Space);
            let gs = app.handle().global_shortcut();
            if gs.register(alt_space).is_err() {
                let alt_cmd_space = Shortcut::new(Some(Modifiers::ALT | Modifiers::META), Code::Space);
                gs.register(alt_cmd_space)?;
            }

            Ok(())
        })
        .manage(AppState {
            registry: Arc::new(registry),
            chat_sub: TokioMutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            search_capabilities,
            get_plugin_info,
            render_ui,
            dispatch_action,
            read_file,
            write_file,
            stat_file,
            preview_file_slice,
            open_in_editor,
            set_window_mode,
            chat_subscribe,
            chat_unsubscribe,
        ])
        .run(tauri::generate_context!())
        .expect("error while running dust desktop");
}
