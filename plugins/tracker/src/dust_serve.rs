//! dust-serve — DustPlugin implementation for the tracker binary.
//!
//! Exposes the tracker issue database to the Nanika dust dashboard via the
//! dust wire protocol over a Unix socket.  Implements the full protocol
//! manually (not via `dust_sdk::run()`) to support:
//!
//! - `events.subscribe` / `events.unsubscribe` (REPLAY-04/05/06/08)
//! - Live push of `data_updated` events to active subscribers (REPLAY-06)
//! - `cancel` for deferred actions (CANCEL-03/05/06)
//! - `Refresh` events from host → re-emit full issue list (data_updated)
//! - Heartbeat echo + proactive heartbeat + miss detection (HEARTBEAT-01/02)
//!
//! ## Connection modes
//!
//! The first accepted connection is the **lifecycle connection** (registry's
//! background task): it performs the ready/host_info handshake and runs the
//! full heartbeat + subscription loop.  All subsequent connections are
//! **IPC connections** (from `ipc_call` in dust-registry): they skip the
//! handshake and handle a single request/response immediately.
//!
//! ## Mutation broadcast
//!
//! When an IPC connection executes a mutation, it sends a `data_updated` event
//! to a shared `broadcast::Sender`.  The lifecycle loop receives these events
//! and writes them to the lifecycle stream, where `plugin_active_task` picks
//! them up and broadcasts them to all dashboard subscribers.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dust_core::events::EventRing;
use dust_sdk::{
    ActionParams, ActionResult, Capability, Component, Envelope, ErrorObject, EventEnvelope,
    EventType, HeartbeatEnvelope, PluginManifest, ResponseEnvelope, TableColumn,
};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

// ── Constants ────────────────────────────────────────────────────────────────

// Single source of truth lives in the `tracker` lib crate (`lib.rs`); the
// `plugin_id` conformance test asserts it equals `plugin.json["name"]`.
// The registry computes its expected socket path as `<runtime_dir>/<name>.sock`,
// so both sides must match or the registry won't find the plugin socket.
use tracker::PLUGIN_ID;

const PROTOCOL_VERSION: &str = "1.0.0";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_FRAME_SIZE: usize = 1 << 20;
const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 30_000;
const HEARTBEAT_MISS_COUNT: u32 = 3;
const MUTATION_BROADCAST_CAP: usize = 256;

// ── Sequence counter ─────────────────────────────────────────────────────────

static SEQ: AtomicU64 = AtomicU64::new(1);

fn next_seq() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

// ── Lifecycle flag ────────────────────────────────────────────────────────────
//
// Only the FIRST accepted connection goes through the full handshake and runs
// the heartbeat/subscription loop.  Subsequent connections (from ipc_call) skip
// ready/host_info and handle a single request.

static LIFECYCLE_DONE: AtomicBool = AtomicBool::new(false);

// ── Timestamp ────────────────────────────────────────────────────────────────

fn utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let epoch_secs = dur.as_secs();
    let ms = dur.subsec_millis() as u64;
    let (y, mo, d) = civil_from_days((epoch_secs / 86400) as i64);
    let tod = epoch_secs % 86400;
    let h = tod / 3600;
    let mi = (tod % 3600) / 60;
    let s = tod % 60;
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{ms:03}Z")
}

fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719_468;
    let era: i64 = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ── Runtime directory ────────────────────────────────────────────────────────

fn runtime_dir() -> std::io::Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(xdg).join("nanika").join("plugins"));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set"))?;
    Ok(home.join(".alluka").join("run").join("plugins"))
}

// ── Frame I/O ────────────────────────────────────────────────────────────────

enum FrameResult {
    Frame(Envelope),
    ZeroLength,
    Closed,
}

async fn read_frame_ex(stream: &mut UnixStream) -> std::io::Result<FrameResult> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Ok(FrameResult::Closed);
        }
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(FrameResult::ZeroLength);
    }
    if len > MAX_FRAME_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame size {len} exceeds MAX_FRAME_SIZE"),
        ));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    serde_json::from_slice(&buf)
        .map(FrameResult::Frame)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

async fn read_frame_handshake(stream: &mut UnixStream) -> std::io::Result<Option<Envelope>> {
    loop {
        match read_frame_ex(stream).await? {
            FrameResult::Frame(env) => return Ok(Some(env)),
            FrameResult::ZeroLength => continue,
            FrameResult::Closed => return Ok(None),
        }
    }
}

async fn write_frame(stream: &mut UnixStream, env: &Envelope) -> std::io::Result<()> {
    let payload = serde_json::to_vec(env)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let len = (payload.len() as u32).to_be_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&payload).await?;
    stream.flush().await
}

// ── Plugin manifest ──────────────────────────────────────────────────────────

fn make_manifest() -> PluginManifest {
    PluginManifest {
        name: "Tracker".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        description:
            "Local issue tracker with hierarchical relationships and priority-based ready detection."
                .into(),
        capabilities: vec![
            Capability::Widget { refresh_secs: 30 },
            Capability::Command {
                prefix: "tracker".into(),
            },
        ],
        icon: Some("ListCheck".into()),
    }
}

fn make_ready_event() -> Envelope {
    let seq = next_seq();
    let manifest = make_manifest();
    Envelope::Event(EventEnvelope {
        id: format!("evt_{seq:016x}"),
        event_type: EventType::Ready,
        ts: utc_now(),
        sequence: Some(seq),
        data: serde_json::json!({
            "manifest": serde_json::to_value(&manifest).unwrap_or(Value::Null),
            "protocol_version": PROTOCOL_VERSION,
            "plugin_info": {
                "pid": std::process::id(),
                "started_at": utc_now(),
            }
        }),
    })
}

// ── Tool registration with chat plugin ───────────────────────────────────────

/// Build the six tool declarations the tracker offers to the chat plugin.
///
/// Each entry mirrors the `ToolDeclInput` wire shape defined by
/// `plugins/chat/src/tools.rs` — `{tool: {name, description, input_schema},
/// dispatch: {op_id, static_item_id?, timeout_ms?}}`. Routing is driven by
/// `op_id`; `static_item_id` is only set for ops that are genuinely singleton
/// (no per-call target row). For `tracker.update` and `tracker.delete` we
/// leave `static_item_id` unset so the per-call `issue_id` supplied by the
/// model reaches the handler through `ActionParams.args` (the chat
/// dispatcher would otherwise clobber it with the static override — see
/// `plugins/chat/src/tools.rs::ToolDispatch`).
fn tracker_tool_declarations() -> Vec<Value> {
    let empty_object_schema = serde_json::json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    });

    // Helper for singleton ops (no per-call target): static_item_id = op_id.
    let singleton_decl = |name: &str, desc: &str, schema: Value, op_id: &str| {
        serde_json::json!({
            "tool": {
                "name": name,
                "description": desc,
                "input_schema": schema,
            },
            "dispatch": {
                "op_id": op_id,
                "static_item_id": op_id,
                "timeout_ms": 30_000,
            },
        })
    };

    // Helper for targeted ops (update/delete): no static_item_id — the
    // target row id flows through `args.issue_id`.
    let targeted_decl = |name: &str, desc: &str, schema: Value, op_id: &str| {
        serde_json::json!({
            "tool": {
                "name": name,
                "description": desc,
                "input_schema": schema,
            },
            "dispatch": {
                "op_id": op_id,
                "timeout_ms": 30_000,
            },
        })
    };

    vec![
        singleton_decl(
            "tracker.next",
            "Return the single next ready tracker issue by priority order.",
            empty_object_schema.clone(),
            "next",
        ),
        singleton_decl(
            "tracker.ready",
            "List all ready tracker issues (no blockers, status=open).",
            empty_object_schema.clone(),
            "ready",
        ),
        singleton_decl(
            "tracker.tree",
            "Return the full tracker issue tree (all statuses).",
            empty_object_schema,
            "tree",
        ),
        singleton_decl(
            "tracker.create",
            "Create a new tracker issue with the given title.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Issue title" }
                },
                "required": ["title"],
                "additionalProperties": false
            }),
            "create",
        ),
        targeted_decl(
            "tracker.update",
            "Update a tracker issue's status (e.g. open, in_progress, done).",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "issue_id": { "type": "string", "description": "Issue id, e.g. TRK-123" },
                    "status": { "type": "string", "description": "New status" }
                },
                "required": ["issue_id", "status"],
                "additionalProperties": false
            }),
            "update",
        ),
        targeted_decl(
            "tracker.delete",
            "Delete a tracker issue by id.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "issue_id": { "type": "string", "description": "Issue id, e.g. TRK-123" }
                },
                "required": ["issue_id"],
                "additionalProperties": false
            }),
            "delete",
        ),
    ]
}

/// Attempt to call `tools.register` on the chat plugin.
///
/// Returns `Ok(())` on success, `Err` otherwise. Caller is responsible for
/// any retry loop — this is one connect/handshake/request/close pass.
async fn call_tools_register_once(chat_socket: &Path) -> Result<(), String> {
    let mut stream = UnixStream::connect(chat_socket)
        .await
        .map_err(|e| format!("connect {}: {e}", chat_socket.display()))?;

    // 1. Expect `ready` event from the chat plugin.
    let ready = tokio::time::timeout(Duration::from_secs(5), read_frame_handshake(&mut stream))
        .await
        .map_err(|_| "timeout waiting for chat ready".to_string())?
        .map_err(|e| format!("read ready: {e}"))?;
    match ready {
        Some(Envelope::Event(e)) if e.event_type == EventType::Ready => {}
        Some(other) => return Err(format!("expected ready event, got {other:?}")),
        None => return Err("chat closed before sending ready".into()),
    }

    // 2. Send `host_info` event so chat enters its dispatch loop.
    let seq = next_seq();
    let host_info = Envelope::Event(EventEnvelope {
        id: format!("evt_{seq:016x}"),
        event_type: EventType::HostInfo,
        ts: utc_now(),
        sequence: Some(seq),
        data: serde_json::json!({
            "host": "tracker-bootstrap",
            "protocol_version": PROTOCOL_VERSION,
        }),
    });
    write_frame(&mut stream, &host_info)
        .await
        .map_err(|e| format!("write host_info: {e}"))?;

    // 3. Send `tools.register` request.
    let decls = tracker_tool_declarations();
    let mut args: HashMap<String, Value> = HashMap::new();
    args.insert(
        "owner_plugin_id".into(),
        Value::String(PLUGIN_ID.to_string()),
    );
    args.insert("tools".into(), Value::Array(decls));

    let params = ActionParams {
        op_id: None,
        item_id: Some("tools.register".into()),
        args,
    };
    let req_id = format!("req_{:016x}", next_seq());
    let req = Envelope::Request(dust_sdk::RequestEnvelope {
        id: req_id.clone(),
        method: "action".into(),
        params: serde_json::to_value(&params)
            .map_err(|e| format!("serialize ActionParams: {e}"))?,
    });
    write_frame(&mut stream, &req)
        .await
        .map_err(|e| format!("write request: {e}"))?;

    // 4. Read response.
    let resp = tokio::time::timeout(Duration::from_secs(5), read_frame_handshake(&mut stream))
        .await
        .map_err(|_| "timeout waiting for tools.register response".to_string())?
        .map_err(|e| format!("read response: {e}"))?;

    match resp {
        Some(Envelope::Response(r)) if r.id == req_id => {
            if let Some(err) = r.error {
                return Err(format!("tools.register rejected: {}", err.message));
            }
            // Check the ActionResult body.
            let body = r.result.unwrap_or(Value::Null);
            let success = body
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !success {
                let msg = body
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                return Err(format!("tools.register failed: {msg}"));
            }
            Ok(())
        }
        Some(other) => Err(format!("unexpected envelope: {other:?}")),
        None => Err("chat closed before response".into()),
    }
}

/// Spawn a background task that registers tracker's tools with the chat plugin.
///
/// Retries with a short backoff since the chat plugin may not be up yet.
/// Failures are logged but do not block tracker startup.
pub fn spawn_register_tools_with_chat() {
    tokio::spawn(async move {
        let socket_dir = match runtime_dir() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("dust-tracker: tools.register: runtime dir: {e}");
                return;
            }
        };
        let chat_socket = socket_dir.join("chat.sock");

        let attempts: &[Duration] = &[
            Duration::from_millis(500),
            Duration::from_secs(2),
            Duration::from_secs(5),
            Duration::from_secs(10),
        ];

        for (i, delay) in attempts.iter().enumerate() {
            tokio::time::sleep(*delay).await;
            match call_tools_register_once(&chat_socket).await {
                Ok(()) => {
                    eprintln!(
                        "dust-tracker: registered 6 tools with chat plugin (attempt {})",
                        i + 1
                    );
                    return;
                }
                Err(e) => {
                    if i + 1 == attempts.len() {
                        eprintln!(
                            "dust-tracker: tools.register failed after {} attempts: {e}",
                            attempts.len()
                        );
                    }
                }
            }
        }
    });
}

// ── Blocking helpers ─────────────────────────────────────────────────────────

fn render_issues(db_path: &Path) -> Result<Vec<Component>, String> {
    let conn = crate::db::open(db_path).map_err(|e| e.to_string())?;

    let mut all_issues =
        crate::commands::list(&conn, Some("open"), None).map_err(|e| e.to_string())?;
    let in_progress =
        crate::commands::list(&conn, Some("in-progress"), None).map_err(|e| e.to_string())?;
    let blocked =
        crate::commands::list(&conn, Some("blocked"), None).map_err(|e| e.to_string())?;

    let total_count = all_issues.len() + in_progress.len() + blocked.len();
    let in_progress_count = in_progress.len();
    let blocked_count = blocked.len();

    all_issues.extend(in_progress);
    all_issues.extend(blocked);
    all_issues.sort_by_key(|i| i.seq_id.unwrap_or(i64::MAX));

    let ready_issues = crate::commands::ready(&conn).map_err(|e| e.to_string())?;
    let ready_count = ready_issues.len();

    let summary = Component::KeyValue {
        pairs: vec![
            dust_sdk::KVPair::new("Total", total_count.to_string()),
            dust_sdk::KVPair::new("Ready", ready_count.to_string())
                .with_color(dust_sdk::Color::new(0, 200, 0)),
            dust_sdk::KVPair::new("In Progress", in_progress_count.to_string())
                .with_color(dust_sdk::Color::new(255, 165, 0)),
            dust_sdk::KVPair::new("Blocked", blocked_count.to_string())
                .with_color(dust_sdk::Color::new(255, 0, 0)),
        ],
    };

    let divider1 = Component::Divider;

    let columns = vec![
        TableColumn::new("#").with_width(6),
        TableColumn::new("St").with_width(3),
        TableColumn::new("Pri").with_width(3),
        TableColumn::new("Title").with_width(40),
        TableColumn::new("Assn").with_width(8),
        TableColumn::new("Upd").with_width(10),
    ];

    let rows = all_issues
        .iter()
        .map(|issue| {
            let id = issue
                .seq_id
                .map(|n| format!("TRK-{n}"))
                .unwrap_or_else(|| issue.id[..8.min(issue.id.len())].to_string());

            let status_glyph = match issue.status.as_str() {
                "open" => "○",
                "in-progress" => "◐",
                "done" => "✓",
                "cancelled" => "✗",
                _ => "?",
            };

            let priority = issue.priority.clone().unwrap_or_else(|| "-".into());
            let assignee = issue.assignee.clone().unwrap_or_else(|| "-".into());
            let updated = format_relative_time(&issue.updated_at);

            vec![
                id,
                status_glyph.into(),
                priority,
                issue.title.clone(),
                assignee,
                updated,
            ]
        })
        .collect();

    let table = Component::Table { columns, rows };
    let divider2 = Component::Divider;

    let mut legend_items = vec![
        dust_sdk::ListItem::new("p0", "P0 — Critical"),
        dust_sdk::ListItem::new("p1", "P1 — High"),
        dust_sdk::ListItem::new("p2", "P2 — Medium"),
        dust_sdk::ListItem::new("p3", "P3 — Low"),
        dust_sdk::ListItem::new("open", "○ Open"),
        dust_sdk::ListItem::new("in-progress", "◐ In Progress"),
    ];
    for item in &mut legend_items {
        item.disabled = true;
    }

    let legend = Component::List {
        items: legend_items,
        title: Some("Legend".into()),
    };

    Ok(vec![summary, divider1, table, divider2, legend])
}

fn format_relative_time(rfc3339: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    if let Ok(dur) = chrono::DateTime::parse_from_rfc3339(rfc3339) {
        let then = dur.timestamp();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let elapsed_secs = (now - then).max(0);

        if elapsed_secs < 60 {
            "now".to_string()
        } else if elapsed_secs < 3600 {
            format!("{}m", elapsed_secs / 60)
        } else if elapsed_secs < 86400 {
            format!("{}h", elapsed_secs / 3600)
        } else if elapsed_secs < 604800 {
            format!("{}d", elapsed_secs / 86400)
        } else if elapsed_secs < 2592000 {
            format!("{}w", elapsed_secs / 604800)
        } else {
            format!("{}mo", elapsed_secs / 2592000)
        }
    } else {
        "-".to_string()
    }
}

fn dispatch_action(
    db_path: &Path,
    op_id: &str,
    item_id: Option<&str>,
    args: &HashMap<String, Value>,
) -> (ActionResult, bool) {
    let conn = match crate::db::open(db_path) {
        Ok(c) => c,
        Err(e) => {
            return (
                ActionResult::err(format!("failed to open tracker db: {e}")),
                false,
            )
        }
    };

    match op_id {
        "next" => match crate::commands::next(&conn) {
            Ok(Some(issue)) => {
                let id = issue
                    .seq_id
                    .map(|n| format!("TRK-{n}"))
                    .unwrap_or_else(|| issue.id.clone());
                (
                    ActionResult {
                        success: true,
                        message: Some(format!("Next: [{id}] {}", issue.title)),
                        data: Some(serde_json::to_value(&issue).unwrap_or(Value::Null)),
                    },
                    false,
                )
            }
            Ok(None) => (ActionResult::ok_with("No ready issues."), false),
            Err(e) => (ActionResult::err(format!("next failed: {e}")), false),
        },

        "ready" => match crate::commands::ready(&conn) {
            Ok(issues) => (
                ActionResult {
                    success: true,
                    message: Some(format!("{} ready issue(s)", issues.len())),
                    data: Some(serde_json::to_value(&issues).unwrap_or(Value::Null)),
                },
                false,
            ),
            Err(e) => (ActionResult::err(format!("ready failed: {e}")), false),
        },

        "tree" => match crate::commands::list(&conn, None, None) {
            Ok(issues) => (
                ActionResult {
                    success: true,
                    message: Some(format!("{} total issue(s)", issues.len())),
                    data: Some(serde_json::to_value(&issues).unwrap_or(Value::Null)),
                },
                false,
            ),
            Err(e) => (ActionResult::err(format!("tree/list failed: {e}")), false),
        },

        "create" => {
            let title = match args.get("title").and_then(Value::as_str) {
                Some(t) => t,
                None => return (ActionResult::err("create requires args.title"), false),
            };
            let priority = args.get("priority").and_then(Value::as_str);
            let description = args.get("description").and_then(Value::as_str);
            let assignee = args.get("assignee").and_then(Value::as_str);
            let labels = args.get("labels").and_then(Value::as_str);
            let parent = args.get("parent").and_then(Value::as_str);

            match crate::commands::create(
                &conn,
                title,
                priority,
                description,
                assignee,
                labels,
                parent,
            ) {
                Ok(issue) => {
                    let id = issue
                        .seq_id
                        .map(|n| format!("TRK-{n}"))
                        .unwrap_or_else(|| issue.id.clone());
                    (
                        ActionResult {
                            success: true,
                            message: Some(format!("created {id}")),
                            data: Some(
                                serde_json::to_value(&issue).unwrap_or(Value::Null),
                            ),
                        },
                        true,
                    )
                }
                Err(e) => (ActionResult::err(format!("create failed: {e}")), false),
            }
        }

        "update" => {
            // Prefer args.issue_id (model-supplied target) over item_id. The
            // chat-plugin dispatcher passes issue_id through when the tool
            // decl omits `static_item_id`; item_id is only set for legacy
            // CLI callers that route a row id through ActionParams directly.
            let id = match args
                .get("issue_id")
                .and_then(Value::as_str)
                .or(item_id)
            {
                Some(i) => i,
                None => {
                    return (
                        ActionResult::err("update requires args.issue_id or item_id"),
                        false,
                    )
                }
            };
            let title = args.get("title").and_then(Value::as_str);
            let status = args.get("status").and_then(Value::as_str);
            let priority = args.get("priority").and_then(Value::as_str);
            let description = args.get("description").and_then(Value::as_str);
            let assignee = args.get("assignee").and_then(Value::as_str);
            let labels = args.get("labels").and_then(Value::as_str);

            match crate::commands::update(
                &conn, id, title, status, priority, description, assignee, labels,
            ) {
                Ok(issue) => {
                    let display_id = issue
                        .seq_id
                        .map(|n| format!("TRK-{n}"))
                        .unwrap_or_else(|| issue.id.clone());
                    (
                        ActionResult {
                            success: true,
                            message: Some(format!("updated {display_id}")),
                            data: Some(
                                serde_json::to_value(&issue).unwrap_or(Value::Null),
                            ),
                        },
                        true,
                    )
                }
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    (ActionResult::err(format!("issue {id} not found")), false)
                }
                Err(e) => (ActionResult::err(format!("update failed: {e}")), false),
            }
        }

        "delete" => {
            let id = match args
                .get("issue_id")
                .and_then(Value::as_str)
                .or(item_id)
            {
                Some(i) => i,
                None => {
                    return (
                        ActionResult::err("delete requires args.issue_id or item_id"),
                        false,
                    )
                }
            };
            match crate::commands::delete(&conn, id) {
                Ok(()) => (
                    ActionResult {
                        success: true,
                        message: Some(format!("deleted {id}")),
                        data: None,
                    },
                    true,
                ),
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    (ActionResult::err(format!("issue {id} not found")), false)
                }
                Err(e) => (ActionResult::err(format!("delete failed: {e}")), false),
            }
        }

        other => (
            ActionResult::err(format!(
                "unknown op_id: {other:?}. supported: next, ready, tree, create, update, delete"
            )),
            false,
        ),
    }
}

fn build_refresh_snapshot(db_path: &Path) -> Result<Value, String> {
    let conn = crate::db::open(db_path).map_err(|e| e.to_string())?;
    let mut issues =
        crate::commands::list(&conn, Some("open"), None).map_err(|e| e.to_string())?;
    let in_progress =
        crate::commands::list(&conn, Some("in-progress"), None).map_err(|e| e.to_string())?;
    issues.extend(in_progress);
    issues.sort_by_key(|i| i.seq_id.unwrap_or(i64::MAX));
    serde_json::to_value(&issues).map_err(|e| e.to_string())
}

// ── IPC connection handler (non-lifecycle, single request) ────────────────────

async fn handle_ipc_request(
    stream: &mut UnixStream,
    db_path: &Path,
    req: dust_sdk::RequestEnvelope,
    mutation_tx: &Arc<broadcast::Sender<EventEnvelope>>,
) -> std::io::Result<()> {
    match req.method.as_str() {
        "render" => {
            let db_path2 = db_path.to_path_buf();
            let components = tokio::task::spawn_blocking(move || render_issues(&db_path2))
                .await
                .unwrap_or_else(|e| Err(format!("render task panicked: {e}")));
            let resp = match components {
                Ok(comps) => ResponseEnvelope {
                    id: req.id,
                    result: Some(
                        serde_json::to_value(&comps).unwrap_or(serde_json::json!([])),
                    ),
                    error: None,
                },
                Err(e) => ResponseEnvelope {
                    id: req.id,
                    result: Some(serde_json::json!([{
                        "type": "text",
                        "content": format!("tracker error: {e}"),
                        "style": {}
                    }])),
                    error: None,
                },
            };
            write_frame(stream, &Envelope::Response(resp)).await
        }

        "action" => {
            let params: ActionParams =
                serde_json::from_value(req.params.clone()).unwrap_or_default();
            let op_id = params.op_id.as_deref().unwrap_or("").to_string();
            let item_id = params.item_id.clone();
            let args = params.args.clone();
            let db_path2 = db_path.to_path_buf();

            let (result, is_mutation) = tokio::task::spawn_blocking(move || {
                dispatch_action(&db_path2, &op_id, item_id.as_deref(), &args)
            })
            .await
            .unwrap_or_else(|e| {
                (
                    ActionResult::err(format!("tracker: action task panicked: {e}")),
                    false,
                )
            });

            let result_value = serde_json::to_value(&result).unwrap_or(Value::Null);
            let resp = ResponseEnvelope {
                id: req.id,
                result: Some(result_value),
                error: None,
            };
            write_frame(stream, &Envelope::Response(resp)).await?;

            if is_mutation {
                let event_seq = next_seq();
                let mutation_data = result
                    .data
                    .unwrap_or_else(|| serde_json::json!({"source": "mutation"}));
                let data_updated = EventEnvelope {
                    id: format!("evt_{event_seq:016x}"),
                    event_type: EventType::DataUpdated,
                    ts: utc_now(),
                    sequence: Some(event_seq),
                    data: mutation_data,
                };
                // Broadcast to lifecycle connection for relay to subscribers.
                let _ = mutation_tx.send(data_updated);
            }

            Ok(())
        }

        "manifest" | "refresh_manifest" => {
            let m = make_manifest();
            let resp = ResponseEnvelope {
                id: req.id,
                result: Some(serde_json::to_value(&m).unwrap_or(Value::Null)),
                error: None,
            };
            write_frame(stream, &Envelope::Response(resp)).await
        }

        method => {
            let resp = ResponseEnvelope {
                id: req.id,
                result: None,
                error: Some(ErrorObject {
                    code: -32601,
                    message: format!("method not found: {method}"),
                    data: None,
                }),
            };
            write_frame(stream, &Envelope::Response(resp)).await
        }
    }
}

// ── Lifecycle connection handler ──────────────────────────────────────────────

async fn run_lifecycle(
    mut stream: UnixStream,
    db_path: PathBuf,
    mutation_tx: Arc<broadcast::Sender<EventEnvelope>>,
) -> std::io::Result<()> {
    let hb_interval_ms: u64 = std::env::var("DUST_HEARTBEAT_INTERVAL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL_MS);
    let hb_interval = Duration::from_millis(hb_interval_ms);
    let miss_deadline = hb_interval * HEARTBEAT_MISS_COUNT;

    let mut last_registry_hb = tokio::time::Instant::now();
    let mut next_hb = tokio::time::Instant::now() + hb_interval;
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut ring = EventRing::new();
    let mut active_sub: Option<String> = None;
    let mut mutation_rx = mutation_tx.subscribe();

    loop {
        tokio::select! {
            biased;

            // ── Inbound frame from registry ───────────────────────────────────
            result = tokio::time::timeout_at(next_hb, read_frame_ex(&mut stream)) => {
                match result {
                    Ok(Ok(frame)) => match frame {
                        FrameResult::Closed => return Ok(()),
                        FrameResult::ZeroLength => {}
                        FrameResult::Frame(envelope) => {
                            match envelope {
                                Envelope::Request(req) => {
                                    if !seen_ids.insert(req.id.clone()) {
                                        let err_resp = Envelope::Response(ResponseEnvelope {
                                            id: req.id,
                                            result: None,
                                            error: Some(ErrorObject {
                                                code: -32600,
                                                message: "duplicate request id".into(),
                                                data: None,
                                            }),
                                        });
                                        write_frame(&mut stream, &err_resp).await?;
                                        continue;
                                    }

                                    match req.method.as_str() {
                                        "events.subscribe" => {
                                            if active_sub.is_some() {
                                                let busy = Envelope::Response(ResponseEnvelope {
                                                    id: req.id,
                                                    result: None,
                                                    error: Some(ErrorObject {
                                                        code: -33005,
                                                        message: "busy: subscription already active".into(),
                                                        data: None,
                                                    }),
                                                });
                                                write_frame(&mut stream, &busy).await?;
                                                continue;
                                            }

                                            let since_sequence = req
                                                .params
                                                .get("since_sequence")
                                                .and_then(|v| v.as_u64())
                                                .unwrap_or(0);

                                            match ring.subscribe(since_sequence) {
                                                Err(gap) => {
                                                    let err_resp = Envelope::Response(ResponseEnvelope {
                                                        id: req.id,
                                                        result: None,
                                                        error: Some(ErrorObject {
                                                            code: -33007,
                                                            message: "replay_gap".into(),
                                                            data: Some(serde_json::json!({
                                                                "oldest_available": gap.oldest_available,
                                                                "requested": gap.requested,
                                                            })),
                                                        }),
                                                    });
                                                    write_frame(&mut stream, &err_resp).await?;
                                                }
                                                Ok(events) => {
                                                    let sub_id = format!("sub_{:016x}", next_seq());
                                                    active_sub = Some(sub_id.clone());
                                                    let next_seq_val = ring.next_sequence();
                                                    let events_json = serde_json::to_value(&events)
                                                        .unwrap_or(serde_json::json!([]));
                                                    let resp = Envelope::Response(ResponseEnvelope {
                                                        id: req.id,
                                                        result: Some(serde_json::json!({
                                                            "subscription_id": sub_id,
                                                            "events": events_json,
                                                            "next_sequence": next_seq_val,
                                                        })),
                                                        error: None,
                                                    });
                                                    write_frame(&mut stream, &resp).await?;
                                                }
                                            }
                                        }

                                        "events.unsubscribe" => {
                                            active_sub = None;
                                            let resp = Envelope::Response(ResponseEnvelope {
                                                id: req.id,
                                                result: Some(serde_json::json!({"ok": true})),
                                                error: None,
                                            });
                                            write_frame(&mut stream, &resp).await?;
                                        }

                                        "cancel" => {
                                            let resp = Envelope::Response(ResponseEnvelope {
                                                id: req.id,
                                                result: Some(serde_json::json!({"ok": true, "already_complete": true})),
                                                error: None,
                                            });
                                            write_frame(&mut stream, &resp).await?;
                                        }

                                        "action" => {
                                            let params: ActionParams =
                                                serde_json::from_value(req.params.clone())
                                                    .unwrap_or_default();
                                            let action_op_id =
                                                params.op_id.as_deref().unwrap_or("").to_string();
                                            let action_item_id = params.item_id.clone();
                                            let action_args = params.args.clone();
                                            let action_db_path = db_path.clone();

                                            let (result, is_mutation) =
                                                match tokio::task::spawn_blocking(move || {
                                                    dispatch_action(
                                                        &action_db_path,
                                                        &action_op_id,
                                                        action_item_id.as_deref(),
                                                        &action_args,
                                                    )
                                                })
                                                .await
                                                {
                                                    Ok(r) => r,
                                                    Err(e) => (
                                                        ActionResult::err(format!(
                                                            "tracker: action task panicked: {e}"
                                                        )),
                                                        false,
                                                    ),
                                                };

                                            let result_value =
                                                serde_json::to_value(&result).unwrap_or(Value::Null);
                                            let resp = Envelope::Response(ResponseEnvelope {
                                                id: req.id,
                                                result: Some(result_value),
                                                error: None,
                                            });
                                            write_frame(&mut stream, &resp).await?;

                                            if is_mutation {
                                                let event_seq = next_seq();
                                                let mutation_data = result.data.unwrap_or_else(
                                                    || serde_json::json!({"source": "action"}),
                                                );
                                                let data_updated = EventEnvelope {
                                                    id: format!("evt_{event_seq:016x}"),
                                                    event_type: EventType::DataUpdated,
                                                    ts: utc_now(),
                                                    sequence: Some(event_seq),
                                                    data: mutation_data.clone(),
                                                };
                                                ring.push(data_updated.clone());
                                                if active_sub.is_some() {
                                                    write_frame(
                                                        &mut stream,
                                                        &Envelope::Event(data_updated),
                                                    )
                                                    .await?;
                                                }
                                            }
                                        }

                                        "manifest" | "refresh_manifest" => {
                                            let m = make_manifest();
                                            let resp = Envelope::Response(ResponseEnvelope {
                                                id: req.id,
                                                result: Some(
                                                    serde_json::to_value(&m).unwrap_or(Value::Null),
                                                ),
                                                error: None,
                                            });
                                            write_frame(&mut stream, &resp).await?;
                                        }

                                        "render" => {
                                            let render_db_path = db_path.clone();
                                            let components = match tokio::task::spawn_blocking(
                                                move || render_issues(&render_db_path),
                                            )
                                            .await
                                            {
                                                Ok(Ok(c)) => c,
                                                Ok(Err(e)) => vec![Component::Text {
                                                    content: format!("tracker: error: {e}"),
                                                    style: Default::default(),
                                                }],
                                                Err(e) => vec![Component::Text {
                                                    content: format!("tracker: render panicked: {e}"),
                                                    style: Default::default(),
                                                }],
                                            };
                                            let resp = Envelope::Response(ResponseEnvelope {
                                                id: req.id,
                                                result: Some(
                                                    serde_json::to_value(&components)
                                                        .unwrap_or(serde_json::json!([])),
                                                ),
                                                error: None,
                                            });
                                            write_frame(&mut stream, &resp).await?;
                                        }

                                        method => {
                                            let err = Envelope::Response(ResponseEnvelope {
                                                id: req.id,
                                                result: None,
                                                error: Some(ErrorObject {
                                                    code: -32601,
                                                    message: format!("method not found: {method}"),
                                                    data: None,
                                                }),
                                            });
                                            write_frame(&mut stream, &err).await?;
                                        }
                                    }
                                }

                                Envelope::Heartbeat(_) => {
                                    last_registry_hb = tokio::time::Instant::now();
                                    let hb = Envelope::Heartbeat(HeartbeatEnvelope { ts: utc_now() });
                                    write_frame(&mut stream, &hb).await?;
                                }

                                Envelope::Shutdown(_) => return Ok(()),

                                Envelope::Event(ref e) if e.event_type == EventType::Refresh => {
                                    let refresh_db_path = db_path.clone();
                                    let snapshot = tokio::task::spawn_blocking(move || {
                                        build_refresh_snapshot(&refresh_db_path)
                                    })
                                    .await;

                                    let data = match snapshot {
                                        Ok(Ok(v)) => v,
                                        Ok(Err(e)) => serde_json::json!({"error": e}),
                                        Err(e) => serde_json::json!({"error": format!("panic: {e}")}),
                                    };

                                    let event_seq = next_seq();
                                    let data_updated = EventEnvelope {
                                        id: format!("evt_{event_seq:016x}"),
                                        event_type: EventType::DataUpdated,
                                        ts: utc_now(),
                                        sequence: Some(event_seq),
                                        data: serde_json::json!({
                                            "source": "refresh",
                                            "issues": data,
                                        }),
                                    };
                                    ring.push(data_updated.clone());

                                    if active_sub.is_some() {
                                        write_frame(&mut stream, &Envelope::Event(data_updated))
                                            .await?;
                                    }
                                }

                                Envelope::Event(_) | Envelope::Response(_) => {}
                            }
                        }
                    },
                    Ok(Err(_)) => return Ok(()),
                    Err(_elapsed) => {
                        if last_registry_hb.elapsed() >= miss_deadline {
                            return Ok(());
                        }
                        let hb = Envelope::Heartbeat(HeartbeatEnvelope { ts: utc_now() });
                        write_frame(&mut stream, &hb).await?;
                        next_hb = tokio::time::Instant::now() + hb_interval;
                    }
                }
            }

            // ── Mutation events from IPC connections ──────────────────────────
            result = mutation_rx.recv() => {
                match result {
                    Ok(event) => {
                        // Push data_updated to lifecycle stream so plugin_active_task
                        // relays it to all dashboard subscribers via broadcast.
                        ring.push(event.clone());
                        if active_sub.is_some() {
                            write_frame(&mut stream, &Envelope::Event(event)).await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {}
                }
            }
        }
    }
}

// ── Top-level connection dispatcher ──────────────────────────────────────────

async fn handle_connection(
    mut stream: UnixStream,
    db_path: PathBuf,
    mutation_tx: Arc<broadcast::Sender<EventEnvelope>>,
) -> std::io::Result<()> {
    // The first accepted connection is the lifecycle connection.
    // All others are short-lived IPC connections from ipc_call.
    let is_lifecycle = !LIFECYCLE_DONE.swap(true, Ordering::SeqCst);

    // Per dust protocol §13, every new connection MUST complete the
    // ready/host_info handshake before any request envelopes may flow,
    // including short-lived subscriber connections that the registry
    // opens for one-shot RPCs (render_ui, dispatch_action).
    write_frame(&mut stream, &make_ready_event()).await?;

    let host_result =
        tokio::time::timeout(HANDSHAKE_TIMEOUT, read_frame_handshake(&mut stream))
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out waiting for host_info",
                )
            })?;

    match host_result? {
        Some(Envelope::Event(ref e)) if e.event_type == EventType::HostInfo => {}
        Some(_) | None => return Ok(()),
    }

    if is_lifecycle {
        run_lifecycle(stream, db_path, mutation_tx).await
    } else {
        match read_frame_handshake(&mut stream).await? {
            Some(Envelope::Request(req)) => {
                handle_ipc_request(&mut stream, &db_path, req, &mutation_tx).await
            }
            _ => Ok(()),
        }
    }
}

// ── Entry point ──────────────────────────────────────────────────────────────

pub fn run(db_path: PathBuf) -> Result<(), String> {
    // Reset lifecycle flag in case the process is reused across tests.
    LIFECYCLE_DONE.store(false, Ordering::SeqCst);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to build tokio runtime: {e}"))?;

    rt.block_on(async {
        let socket_dir = runtime_dir().map_err(|e| format!("runtime dir: {e}"))?;
        tokio::fs::create_dir_all(&socket_dir)
            .await
            .map_err(|e| format!("create socket dir: {e}"))?;

        let socket_path = socket_dir.join(format!("{PLUGIN_ID}.sock"));
        let _ = tokio::fs::remove_file(&socket_path).await;

        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| format!("bind socket: {e}"))?;

        // Shared mutation broadcast: IPC connections send here; lifecycle relays
        // events to the lifecycle stream for plugin_active_task to broadcast.
        let (mutation_tx, _) =
            broadcast::channel::<EventEnvelope>(MUTATION_BROADCAST_CAP);
        let mutation_tx = Arc::new(mutation_tx);

        // Fire-and-forget: tell the chat plugin about our six tools so the
        // agent can invoke them by name. Retries briefly, logs on final failure.
        spawn_register_tools_with_chat();

        loop {
            let (stream, _addr) = listener
                .accept()
                .await
                .map_err(|e| format!("accept: {e}"))?;

            let conn_db_path = db_path.clone();
            let conn_mutation_tx = Arc::clone(&mutation_tx);
            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, conn_db_path, conn_mutation_tx).await {
                    if e.kind() != std::io::ErrorKind::UnexpectedEof
                        && e.kind() != std::io::ErrorKind::TimedOut
                    {
                        eprintln!("dust-tracker: connection error: {e}");
                    }
                }
            });
        }
    })
}

#[cfg(test)]
mod tool_dispatch_tests {
    //! End-to-end tests for the model-facing tool dispatch path. The chat
    //! plugin's `ToolDispatcher` would normally package `ActionParams` from
    //! a Claude-emitted `tool_use`; here we stitch the same `ActionParams`
    //! shape directly against `dispatch_action` so regressions on either
    //! side (tracker or chat) get caught by the tracker suite.

    use super::*;
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn seed_issue(db_path: &Path, title: &str) -> String {
        let conn = crate::db::open(db_path).expect("open db");
        let issue = crate::commands::create(&conn, title, None, None, None, None, None)
            .expect("create issue");
        issue
            .seq_id
            .map(|n| format!("TRK-{n}"))
            .unwrap_or(issue.id)
    }

    /// Mirrors the chat dispatcher: when `static_item_id` is unset the
    /// model-supplied `issue_id` flows through `args`, and `item_id` stays
    /// `None`. The tracker handler must still resolve the target row.
    fn params_from_tool_input(op_id: &str, input: Value) -> (String, Option<String>, HashMap<String, Value>) {
        let op_id = op_id.to_string();
        let item_id = input
            .get("item_id")
            .and_then(Value::as_str)
            .map(String::from);
        let args = match input {
            Value::Object(m) => m
                .into_iter()
                .filter(|(k, _)| k != "item_id")
                .collect::<HashMap<_, _>>(),
            _ => HashMap::new(),
        };
        (op_id, item_id, args)
    }

    #[test]
    fn declarations_omit_static_item_id_for_update_and_delete() {
        let decls = tracker_tool_declarations();
        let by_name: HashMap<&str, &Value> = decls
            .iter()
            .map(|d| (d["tool"]["name"].as_str().unwrap(), d))
            .collect();

        for targeted in ["tracker.update", "tracker.delete"] {
            let d = by_name
                .get(targeted)
                .unwrap_or_else(|| panic!("missing decl for {targeted}"));
            assert!(
                d["dispatch"].get("static_item_id").is_none(),
                "{targeted} must not set static_item_id — it would override \
                 the model-supplied issue_id. decl: {d}",
            );
        }

        // Singletons keep the override.
        for singleton in ["tracker.next", "tracker.ready", "tracker.tree", "tracker.create"] {
            let d = by_name
                .get(singleton)
                .unwrap_or_else(|| panic!("missing decl for {singleton}"));
            assert_eq!(
                d["dispatch"]["static_item_id"],
                d["dispatch"]["op_id"],
                "{singleton} should keep static_item_id = op_id (no per-call target row)",
            );
        }
    }

    #[test]
    fn update_resolves_issue_id_from_args_not_item_id() {
        let tmp = tempdir().expect("tempdir");
        let db_path = tmp.path().join("tracker.db");
        let target = seed_issue(&db_path, "First issue");

        // Model emits `{"issue_id":"TRK-1","status":"done"}`; chat
        // dispatcher (with no static_item_id) forwards that through args
        // and leaves item_id=None.
        let input = serde_json::json!({ "issue_id": target, "status": "done" });
        let (op_id, item_id, args) = params_from_tool_input("update", input);

        let (result, is_mutation) =
            dispatch_action(&db_path, &op_id, item_id.as_deref(), &args);

        assert!(
            result.success,
            "update should succeed via args.issue_id; got: {result:?}",
        );
        assert!(is_mutation, "update must flag as mutation");
        let data = result.data.expect("issue payload present");
        assert_eq!(data.get("status").and_then(Value::as_str), Some("done"));
    }

    #[test]
    fn update_falls_back_to_item_id_for_legacy_callers() {
        // Legacy CLI / dashboard callers still set ActionParams.item_id
        // directly. The handler must keep that path working.
        let tmp = tempdir().expect("tempdir");
        let db_path = tmp.path().join("tracker.db");
        let target = seed_issue(&db_path, "Legacy caller issue");

        let mut args: HashMap<String, Value> = HashMap::new();
        args.insert("status".into(), Value::String("in-progress".into()));

        let (result, is_mutation) =
            dispatch_action(&db_path, "update", Some(&target), &args);

        assert!(result.success, "legacy item_id path must still work: {result:?}");
        assert!(is_mutation);
    }

    #[test]
    fn delete_resolves_issue_id_from_args_not_item_id() {
        let tmp = tempdir().expect("tempdir");
        let db_path = tmp.path().join("tracker.db");
        let target = seed_issue(&db_path, "Issue to delete");

        let input = serde_json::json!({ "issue_id": target });
        let (op_id, item_id, args) = params_from_tool_input("delete", input);

        let (result, is_mutation) =
            dispatch_action(&db_path, &op_id, item_id.as_deref(), &args);

        assert!(
            result.success,
            "delete should succeed via args.issue_id; got: {result:?}",
        );
        assert!(is_mutation);
    }

    #[test]
    fn update_errors_when_no_target_supplied() {
        let tmp = tempdir().expect("tempdir");
        let db_path = tmp.path().join("tracker.db");
        // Don't seed; we just want the "missing target" branch.
        let args: HashMap<String, Value> = HashMap::new();
        let (result, is_mutation) = dispatch_action(&db_path, "update", None, &args);
        assert!(!result.success);
        assert!(!is_mutation);
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("issue_id"),
            "message must hint at args.issue_id: {:?}",
            result.message,
        );
    }
}
