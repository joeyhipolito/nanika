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
//! ## Event ring
//!
//! Mutations (`create`, `update`, `delete`) push `data_updated` events into
//! the per-connection [`EventRing`] from dust-core.  Subscribers receive the
//! ring snapshot on subscribe and live pushes afterward.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dust_core::events::EventRing;
use dust_sdk::{
    ActionParams, ActionResult, Capability, Component, Envelope, ErrorObject, EventEnvelope,
    EventType, HeartbeatEnvelope, PluginManifest, ResponseEnvelope, TableColumn,
};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

// ── Constants ────────────────────────────────────────────────────────────────

const PLUGIN_ID: &str = "dust-tracker";
const PROTOCOL_VERSION: &str = "1.0.0";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_FRAME_SIZE: usize = 1 << 20;
const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 30_000;
const HEARTBEAT_MISS_COUNT: u32 = 3;

// ── Sequence counter ─────────────────────────────────────────────────────────

static SEQ: AtomicU64 = AtomicU64::new(1);

fn next_seq() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

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

// ── Blocking helpers (run inside spawn_blocking) ─────────────────────────────

/// Render all non-cancelled issues as a Table component.
fn render_issues(db_path: &Path) -> Result<Vec<Component>, String> {
    let conn = crate::db::open(db_path).map_err(|e| e.to_string())?;
    let mut issues =
        crate::commands::list(&conn, Some("open"), None).map_err(|e| e.to_string())?;
    let in_progress =
        crate::commands::list(&conn, Some("in-progress"), None).map_err(|e| e.to_string())?;
    issues.extend(in_progress);
    issues.sort_by_key(|i| i.seq_id.unwrap_or(i64::MAX));

    let columns = vec![
        TableColumn::new("ID").with_width(10),
        TableColumn::new("Title").with_width(40),
        TableColumn::new("Status").with_width(12),
        TableColumn::new("Priority").with_width(10),
    ];

    let rows = issues
        .iter()
        .map(|issue| {
            let id = issue
                .seq_id
                .map(|n| format!("TRK-{n}"))
                .unwrap_or_else(|| issue.id[..8.min(issue.id.len())].to_string());
            vec![
                id,
                issue.title.clone(),
                issue.status.clone(),
                issue.priority.clone().unwrap_or_else(|| "-".into()),
            ]
        })
        .collect();

    Ok(vec![Component::Table { columns, rows }])
}

/// Dispatch a tracker action by op_id. Returns (ActionResult, is_mutation).
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
        // ── Read actions ─────────────────────────────────────────────────────
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

        // ── Mutation actions ─────────────────────────────────────────────────
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
            let id = match item_id {
                Some(i) => i,
                None => return (ActionResult::err("update requires item_id"), false),
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
            let id = match item_id {
                Some(i) => i,
                None => return (ActionResult::err("delete requires item_id"), false),
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

/// Build a data_updated snapshot of all open issues for refresh events.
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

// ── Connection handler ───────────────────────────────────────────────────────

async fn handle_connection(mut stream: UnixStream, db_path: PathBuf) -> std::io::Result<()> {
    // ── 1. Send ready event ──────────────────────────────────────────────────
    let seq = next_seq();
    let manifest = make_manifest();
    let ready = Envelope::Event(EventEnvelope {
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
    });
    write_frame(&mut stream, &ready).await?;

    // ── 2. Read host_info (with timeout) ─────────────────────────────────────
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

    // ── 3. Per-connection state ──────────────────────────────────────────────
    let hb_interval_ms: u64 = std::env::var("DUST_HEARTBEAT_INTERVAL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL_MS);
    let hb_interval = Duration::from_millis(hb_interval_ms);
    let miss_deadline = hb_interval * HEARTBEAT_MISS_COUNT;

    let mut last_registry_hb = tokio::time::Instant::now();
    let mut next_hb = tokio::time::Instant::now() + hb_interval;

    // Duplicate request-ID guard (ENVELOPE-06).
    let mut seen_ids: HashSet<String> = HashSet::new();

    // Event ring (REPLAY-15).
    let mut ring = EventRing::new();

    // Active subscription: (subscription_id).
    // Only one per connection (REPLAY-08 / PRESSURE-01).
    let mut active_sub: Option<String> = None;

    // Pending (deferred) actions: op_id → request_id (CANCEL-04/05/06).
    let mut pending_actions: HashMap<String, String> = HashMap::new();

    // ── 4. Dispatch loop ─────────────────────────────────────────────────────
    loop {
        match tokio::time::timeout_at(next_hb, read_frame_ex(&mut stream)).await {
            // ── Frame received ────────────────────────────────────────────────
            Ok(Ok(frame)) => match frame {
                FrameResult::Closed => return Ok(()),
                FrameResult::ZeroLength => {}

                FrameResult::Frame(envelope) => match envelope {
                    Envelope::Request(req) => {
                        // ENVELOPE-06: duplicate IDs.
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
                            // ── events.subscribe ─────────────────────────────
                            "events.subscribe" => {
                                // REPLAY-08 / PRESSURE-01: one subscription per connection.
                                if active_sub.is_some() {
                                    let busy = Envelope::Response(ResponseEnvelope {
                                        id: req.id,
                                        result: None,
                                        error: Some(ErrorObject {
                                            code: -33005,
                                            message: "busy: subscription already active on this connection".into(),
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
                                        // REPLAY-05: replay gap (-33007).
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
                                        // REPLAY-04: subscription_id, events, next_sequence.
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

                            // ── events.unsubscribe ───────────────────────────
                            "events.unsubscribe" => {
                                active_sub = None;
                                let resp = Envelope::Response(ResponseEnvelope {
                                    id: req.id,
                                    result: Some(serde_json::json!({"ok": true})),
                                    error: None,
                                });
                                write_frame(&mut stream, &resp).await?;
                            }

                            // ── cancel ───────────────────────────────────────
                            "cancel" => {
                                let cancel_op_id = req
                                    .params
                                    .get("op_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();

                                if let Some(pending_req_id) =
                                    pending_actions.remove(&cancel_op_id)
                                {
                                    // CANCEL-06: cancel takes effect.
                                    let cancel_ack = Envelope::Response(ResponseEnvelope {
                                        id: req.id,
                                        result: Some(serde_json::json!({
                                            "ok": true,
                                            "already_complete": false,
                                        })),
                                        error: None,
                                    });
                                    write_frame(&mut stream, &cancel_ack).await?;

                                    let canceled = Envelope::Response(ResponseEnvelope {
                                        id: pending_req_id,
                                        result: None,
                                        error: Some(ErrorObject {
                                            code: -33002,
                                            message: "canceled".into(),
                                            data: None,
                                        }),
                                    });
                                    write_frame(&mut stream, &canceled).await?;
                                } else {
                                    // CANCEL-05: operation already complete.
                                    let already_done = Envelope::Response(ResponseEnvelope {
                                        id: req.id,
                                        result: Some(serde_json::json!({
                                            "ok": true,
                                            "already_complete": true,
                                        })),
                                        error: None,
                                    });
                                    write_frame(&mut stream, &already_done).await?;
                                }
                            }

                            // ── action ───────────────────────────────────────
                            "action" => {
                                let params: ActionParams =
                                    serde_json::from_value(req.params.clone())
                                        .unwrap_or_default();

                                // If op_id is present, defer the response (for cancel support).
                                if let Some(ref op_id) = params.op_id {
                                    pending_actions
                                        .insert(op_id.clone(), req.id.clone());
                                    continue;
                                }

                                // Immediate action.
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

                                let resp = Envelope::Response(ResponseEnvelope {
                                    id: req.id,
                                    result: Some(
                                        serde_json::to_value(&result)
                                            .unwrap_or(Value::Null),
                                    ),
                                    error: None,
                                });
                                write_frame(&mut stream, &resp).await?;

                                // Push data_updated event on mutation.
                                if is_mutation {
                                    let event_seq = next_seq();
                                    let data_updated = EventEnvelope {
                                        id: format!("evt_{event_seq:016x}"),
                                        event_type: EventType::DataUpdated,
                                        ts: utc_now(),
                                        sequence: Some(event_seq),
                                        data: result
                                            .data
                                            .unwrap_or(serde_json::json!({"source": "action"})),
                                    };
                                    ring.push(data_updated.clone());

                                    // REPLAY-06: live push to active subscriber.
                                    if active_sub.is_some() {
                                        write_frame(
                                            &mut stream,
                                            &Envelope::Event(data_updated),
                                        )
                                        .await?;
                                    }
                                }
                            }

                            // ── manifest / refresh_manifest ──────────────────
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

                            // ── render ───────────────────────────────────────
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

                            // ── unknown method ───────────────────────────────
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

                    // Handle Refresh events from host.
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

                        // Push to active subscriber.
                        if active_sub.is_some() {
                            write_frame(&mut stream, &Envelope::Event(data_updated))
                                .await?;
                        }
                    }

                    // Other events and stray responses are ignored.
                    Envelope::Event(_) | Envelope::Response(_) => {}
                },
            },

            Ok(Err(_)) => return Ok(()),

            // ── Heartbeat timer fired ────────────────────────────────────────
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
}

// ── Entry point ──────────────────────────────────────────────────────────────

/// Bind the dust socket and serve requests until the process is stopped.
///
/// Called from `main.rs` when the `dust-serve` subcommand is invoked.
pub fn run(db_path: PathBuf) -> Result<(), String> {
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

        loop {
            let (stream, _addr) = listener
                .accept()
                .await
                .map_err(|e| format!("accept: {e}"))?;

            let conn_db_path = db_path.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, conn_db_path).await {
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
