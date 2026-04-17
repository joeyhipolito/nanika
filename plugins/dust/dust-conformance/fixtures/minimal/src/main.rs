//! dust-fixture-minimal — minimal dust plugin for protocol conformance testing.
//!
//! Implements the full dust wire protocol manually (using dust-sdk types but
//! not the `run()` helper) so that it can handle subscribe, cancel, and live
//! event push in addition to the standard `manifest`/`render`/`action` trio.
//!
//! ## Behaviour
//!
//! | Stimulus | Response |
//! |----------|----------|
//! | connection accepted | send `ready` event (sequence=1) |
//! | `host_info` event | begin dispatch loop |
//! | `request: manifest` / `refresh_manifest` | stub `PluginManifest` |
//! | `request: render` | empty component array `[]` |
//! | `request: action` (no `op_id`) | `ActionResult::ok()` + emit `data_updated` event to ring + push to active subscriber |
//! | `request: action` (with `op_id`) | defer response; store in `pending_actions` |
//! | `request: cancel` | ack (`ok`, `already_complete`); if op pending → -33002 on original |
//! | `request: events.subscribe` | snapshot from ring; -33005 if already subscribed; -33007 if replay gap |
//! | `request: events.unsubscribe` | clear active subscription |
//! | `request: <unknown>` | JSON-RPC error -32601 |
//! | `heartbeat` | echo heartbeat |
//! | `shutdown` | return (close connection) |
//!
//! ## Heartbeat
//!
//! The dispatch loop emits one heartbeat per `DUST_HEARTBEAT_INTERVAL_MS`
//! (default: 30 000 ms for non-heartbeat tests, set to a smaller value in
//! heartbeat-specific conformance tests via env var).
//!
//! If no heartbeat is received from the registry for `3 × interval`, the
//! fixture considers the registry dead and closes the connection
//! (HEARTBEAT-02).
//!
//! ## Event Ring
//!
//! `action` calls without an `op_id` emit a `data_updated` event that is
//! appended to the per-connection in-memory ring.  The ring is bounded by
//! `DUST_FIXTURE_MAX_RING_EVENTS` (default 1 000).  Requests for sequences
//! older than the eviction frontier return `-33007 replay_gap` (REPLAY-05).
//!
//! ## Cancellation
//!
//! `action` calls with an `op_id` are queued as pending operations.  The
//! fixture does not respond until either a matching `cancel` arrives (→
//! `-33002 canceled`) or no cancel arrives before the connection closes (in
//! which case the pending response is silently dropped).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

// dust-sdk re-exports the envelope types from dust-core plus the high-level
// component/manifest types — no need for a direct dust-core dependency.
use dust_sdk::{
    ActionResult, Capability, Component, PluginManifest,
    Envelope, ErrorObject, EventEnvelope, EventType, HeartbeatEnvelope,
    ResponseEnvelope,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

// ── Constants ─────────────────────────────────────────────────────────────────

const PLUGIN_ID: &str = "dust-fixture-minimal";
const PROTOCOL_VERSION: &str = "1.0.0";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_FRAME_SIZE: usize = 1 << 20;

/// Default heartbeat interval.  Intentionally large so it never fires during
/// non-heartbeat conformance tests.  Override via `DUST_HEARTBEAT_INTERVAL_MS`.
const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 30_000;

/// Heartbeat miss count before the peer is considered dead (HEARTBEAT-02).
const HEARTBEAT_MISS_COUNT: u32 = 3;

/// Default ring capacity (REPLAY-15).  Override via `DUST_FIXTURE_MAX_RING_EVENTS`
/// to test eviction / replay-gap scenarios without pushing 1 000 events.
const DEFAULT_MAX_RING_EVENTS: usize = 1_000;

// ── Sequence counter ──────────────────────────────────────────────────────────

static SEQ: AtomicU64 = AtomicU64::new(1);

fn next_seq() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

// ── Timestamp ─────────────────────────────────────────────────────────────────

/// ISO 8601 timestamp with millisecond precision (no external dependencies).
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

// ── Runtime directory ─────────────────────────────────────────────────────────

/// Resolve the socket directory, mirroring dust-sdk's `sdk_runtime_dir()`.
fn runtime_dir() -> std::io::Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(xdg).join("nanika").join("plugins"));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set")
        })?;
    Ok(home.join(".alluka").join("run").join("plugins"))
}

// ── Frame I/O ─────────────────────────────────────────────────────────────────

/// Outcome of a single `read_frame_ex` call, distinguishing the three cases
/// the dispatch loop needs to handle differently.
enum FrameResult {
    /// A well-formed Envelope was received.
    Frame(Envelope),
    /// FRAME-04: zero-length frame received — silently discard, keep looping.
    ZeroLength,
    /// FRAME-06: clean EOF on the length prefix — graceful disconnect.
    Closed,
}

/// Read one frame from the stream, returning a [`FrameResult`] that
/// distinguishes zero-length frames (FRAME-04) from clean EOF (FRAME-06).
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

/// Read one frame expecting a specific envelope kind (for handshake only).
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

// ── Plugin manifest ───────────────────────────────────────────────────────────

fn make_manifest() -> PluginManifest {
    PluginManifest {
        name: "Minimal Fixture".into(),
        version: "0.1.0".into(),
        description: "Minimal conformance test fixture for dust-conformance.".into(),
        capabilities: vec![Capability::Widget { refresh_secs: 0 }],
        icon: None,
    }
}

// ── In-memory event ring ──────────────────────────────────────────────────────

/// Push `event` into the ring, evicting the oldest entry if at capacity.
fn ring_push(
    ring: &mut VecDeque<EventEnvelope>,
    evicted_through: &mut Option<u64>,
    max_events: usize,
    event: EventEnvelope,
) {
    while ring.len() >= max_events {
        if let Some(old) = ring.pop_front() {
            if let Some(seq) = old.sequence {
                *evicted_through =
                    Some(evicted_through.map_or(seq, |prev: u64| prev.max(seq)));
            }
        }
    }
    ring.push_back(event);
}

/// Query the ring for a `events.subscribe` request.
///
/// Returns `Ok(events)` on success, or `Err((oldest_available, requested))`
/// for a replay-gap condition (-33007).
fn ring_subscribe(
    ring: &VecDeque<EventEnvelope>,
    evicted_through: Option<u64>,
    since_sequence: u64,
) -> Result<Vec<EventEnvelope>, (u64, u64)> {
    // since_sequence=0 → return all retained events, never a replay gap.
    if since_sequence == 0 {
        return Ok(ring.iter().cloned().collect());
    }

    // Replay gap: subscriber's cursor is behind the eviction window.
    if let Some(evicted) = evicted_through {
        if since_sequence <= evicted {
            let oldest = ring
                .front()
                .and_then(|e| e.sequence)
                .unwrap_or(evicted + 1);
            return Err((oldest, since_sequence));
        }
    }

    // Normal: filter by sequence.
    Ok(ring
        .iter()
        .filter(|e| e.sequence.map_or(false, |s| s >= since_sequence))
        .cloned()
        .collect())
}

/// The sequence number that the next pushed event will carry, i.e. the current
/// value of the global `SEQ` counter (fetch_add already incremented it after
/// the ready event, so the current load is the next available slot).
fn ring_next_sequence() -> u64 {
    SEQ.load(Ordering::Relaxed)
}

// ── Connection handler ────────────────────────────────────────────────────────

async fn handle_connection(mut stream: UnixStream) -> std::io::Result<()> {
    // ── 1. Send ready event ───────────────────────────────────────────────────
    let seq = next_seq();
    let manifest = make_manifest();
    let ready = Envelope::Event(EventEnvelope {
        id: format!("evt_{seq:016x}"),
        event_type: EventType::Ready,
        ts: utc_now(),
        sequence: Some(seq),
        data: serde_json::json!({
            "manifest": serde_json::to_value(&manifest)
                .unwrap_or(serde_json::Value::Null),
            "protocol_version": PROTOCOL_VERSION,
            "plugin_info": {
                "pid": std::process::id(),
                "started_at": utc_now(),
            }
        }),
    });
    write_frame(&mut stream, &ready).await?;

    // ── 2. Read host_info (with timeout) ──────────────────────────────────────
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

    // ── 3. Per-connection state ───────────────────────────────────────────────
    let hb_interval_ms: u64 = std::env::var("DUST_HEARTBEAT_INTERVAL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL_MS);
    let hb_interval = Duration::from_millis(hb_interval_ms);
    let miss_deadline = hb_interval * HEARTBEAT_MISS_COUNT;
    let max_ring_events: usize = std::env::var("DUST_FIXTURE_MAX_RING_EVENTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_RING_EVENTS);

    let mut last_registry_hb = tokio::time::Instant::now();
    let mut next_hb = tokio::time::Instant::now() + hb_interval;

    // Duplicate request-ID guard (ENVELOPE-06).
    let mut seen_ids: HashSet<String> = HashSet::new();

    // Event ring (REPLAY-15).
    let mut ring: VecDeque<EventEnvelope> = VecDeque::new();
    let mut evicted_through: Option<u64> = None;

    // Active subscription: (subscription_id).  Only one per connection
    // (REPLAY-08 / PRESSURE-01).
    let mut active_sub: Option<String> = None;

    // Pending (deferred) actions: op_id → request_id.
    // Used to implement cancellation (CANCEL-04/05/06).
    let mut pending_actions: HashMap<String, String> = HashMap::new();

    // ── 4. Dispatch loop ──────────────────────────────────────────────────────
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
                            // ── events.subscribe ──────────────────────────────
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

                                let since_sequence = req.params
                                    .get("since_sequence")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);

                                match ring_subscribe(&ring, evicted_through, since_sequence) {
                                    Err((oldest_available, requested)) => {
                                        // REPLAY-05: replay gap (-33007).
                                        let err_resp = Envelope::Response(ResponseEnvelope {
                                            id: req.id,
                                            result: None,
                                            error: Some(ErrorObject {
                                                code: -33007,
                                                message: "replay_gap".into(),
                                                data: Some(serde_json::json!({
                                                    "oldest_available": oldest_available,
                                                    "requested": requested,
                                                })),
                                            }),
                                        });
                                        write_frame(&mut stream, &err_resp).await?;
                                    }
                                    Ok(events) => {
                                        // REPLAY-04: subscription_id, events, next_sequence.
                                        let sub_id = format!("sub_{:016x}", next_seq());
                                        active_sub = Some(sub_id.clone());
                                        let next_seq_val = ring_next_sequence();
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

                            // ── events.unsubscribe ────────────────────────────
                            "events.unsubscribe" => {
                                active_sub = None;
                                let resp = Envelope::Response(ResponseEnvelope {
                                    id: req.id,
                                    result: Some(serde_json::json!({"ok": true})),
                                    error: None,
                                });
                                write_frame(&mut stream, &resp).await?;
                            }

                            // ── cancel ────────────────────────────────────────
                            "cancel" => {
                                let op_id = req.params
                                    .get("op_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();

                                if let Some(pending_req_id) = pending_actions.remove(&op_id) {
                                    // CANCEL-06: cancel takes effect → send ack, then -33002.
                                    let cancel_ack = Envelope::Response(ResponseEnvelope {
                                        id: req.id,
                                        result: Some(serde_json::json!({
                                            "ok": true,
                                            "already_complete": false,
                                        })),
                                        error: None,
                                    });
                                    write_frame(&mut stream, &cancel_ack).await?;

                                    // CANCEL-06: the canceled operation returns -33002.
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

                            // ── action ────────────────────────────────────────
                            "action" => {
                                let op_id = req.params
                                    .get("op_id")
                                    .and_then(|v| v.as_str())
                                    .map(String::from);

                                if let Some(op_id) = op_id {
                                    // Deferred action — wait for cancel or connection close.
                                    pending_actions.insert(op_id, req.id);
                                } else {
                                    // Immediate action: respond + emit data_updated event.
                                    let result = ActionResult::ok();
                                    let resp = Envelope::Response(ResponseEnvelope {
                                        id: req.id,
                                        result: Some(
                                            serde_json::to_value(&result)
                                                .unwrap_or(serde_json::Value::Null),
                                        ),
                                        error: None,
                                    });
                                    write_frame(&mut stream, &resp).await?;

                                    // Emit a data_updated event into the ring.
                                    let event_seq = next_seq();
                                    let data_updated = EventEnvelope {
                                        id: format!("evt_{event_seq:016x}"),
                                        event_type: EventType::DataUpdated,
                                        ts: utc_now(),
                                        sequence: Some(event_seq),
                                        data: serde_json::json!({"source": "action"}),
                                    };
                                    ring_push(
                                        &mut ring,
                                        &mut evicted_through,
                                        max_ring_events,
                                        data_updated.clone(),
                                    );

                                    // REPLAY-06: push to active subscriber.
                                    if active_sub.is_some() {
                                        write_frame(
                                            &mut stream,
                                            &Envelope::Event(data_updated),
                                        )
                                        .await?;
                                    }
                                }
                            }

                            // ── manifest / refresh_manifest ───────────────────
                            "manifest" | "refresh_manifest" => {
                                let m = make_manifest();
                                let resp = Envelope::Response(ResponseEnvelope {
                                    id: req.id,
                                    result: Some(
                                        serde_json::to_value(&m)
                                            .unwrap_or(serde_json::Value::Null),
                                    ),
                                    error: None,
                                });
                                write_frame(&mut stream, &resp).await?;
                            }

                            // ── render ────────────────────────────────────────
                            "render" => {
                                let components: Vec<Component> = vec![];
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

                            // ── unknown method ────────────────────────────────
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

                    Envelope::Event(_) | Envelope::Response(_) => {}
                },
            },

            Ok(Err(_)) => return Ok(()),

            // ── Heartbeat timer fired ─────────────────────────────────────────
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

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let socket_dir = runtime_dir()?;
    tokio::fs::create_dir_all(&socket_dir).await?;

    let socket_path = socket_dir.join(format!("{PLUGIN_ID}.sock"));
    let _ = tokio::fs::remove_file(&socket_path).await;

    let listener = UnixListener::bind(&socket_path)?;

    loop {
        let (stream, _addr) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream).await {
                if e.kind() != std::io::ErrorKind::UnexpectedEof
                    && e.kind() != std::io::ErrorKind::TimedOut
                {
                    eprintln!("dust-fixture-minimal: connection error: {e}");
                }
            }
        });
    }
}
