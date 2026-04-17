//! dust-sdk — async plugin runtime for the Nanika dust dashboard system.
//!
//! Implement [`DustPlugin`] on your type and call [`run()`] to bind a Unix
//! socket at the runtime directory (`$XDG_RUNTIME_DIR/nanika/plugins/<id>.sock`
//! or `~/.alluka/run/plugins/<id>.sock`). The runtime:
//!
//! 1. Accepts connections and checks peer credentials (same UID only).
//! 2. Sends a `ready` event with the manifest and protocol version.
//! 3. Reads and validates the `host_info` event from the registry.
//! 4. Enters the dispatch loop — requests, heartbeats, shutdown, and refresh.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
pub use dust_core::{
    ActionResult, BadgeVariant, Capability, Color, Component, KVPair, ListItem, PluginManifest,
    TableColumn, TextStyle,
};
pub use dust_core::envelope::{
    ActionParams, Envelope, ErrorObject, EventEnvelope, EventType, HeartbeatEnvelope,
    RequestEnvelope, ResponseEnvelope, ShutdownEnvelope,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

// ── Protocol constants ────────────────────────────────────────────────────────

/// Protocol version this SDK implements.
const SDK_PROTOCOL_VERSION: &str = "1.0.0";

/// Timeout waiting for the registry's `host_info` event after sending `ready`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

// ── Sequence counter ──────────────────────────────────────────────────────────

static NEXT_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_seq() -> u64 {
    NEXT_SEQ.fetch_add(1, Ordering::Relaxed)
}

// ── Timestamp ─────────────────────────────────────────────────────────────────

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

/// Resolve the socket directory: `$XDG_RUNTIME_DIR/nanika/plugins/` when set,
/// otherwise `~/.alluka/run/plugins/`.  Mirrors the registry's logic
/// (TRANSPORT-01 / TRANSPORT-02).
fn sdk_runtime_dir() -> std::io::Result<PathBuf> {
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

// ── Peer credential check ─────────────────────────────────────────────────────

/// On Linux: read SO_PEERCRED via `getsockopt` and compare the peer uid to
/// the process uid.  Returns `Err` for foreign uids (error -33004 Unauthorized).
#[cfg(target_os = "linux")]
fn check_peer_uid(stream: &UnixStream) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let my_uid = unsafe { libc::getuid() };
    if cred.uid != my_uid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "unauthorized (-33004): peer uid {} != process uid {my_uid}",
                cred.uid
            ),
        ));
    }
    Ok(())
}

/// On macOS: use `getpeereid` to obtain the peer's effective uid.
#[cfg(target_os = "macos")]
fn check_peer_uid(stream: &UnixStream) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut peer_uid: libc::uid_t = 0;
    let mut peer_gid: libc::gid_t = 0;
    let ret = unsafe { libc::getpeereid(fd, &mut peer_uid, &mut peer_gid) };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let my_uid = unsafe { libc::getuid() };
    if peer_uid != my_uid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "unauthorized (-33004): peer uid {peer_uid} != process uid {my_uid}"
            ),
        ));
    }
    Ok(())
}

/// Fallback for platforms that support neither SO_PEERCRED nor getpeereid:
/// accept all connections (no-op).
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn check_peer_uid(_stream: &UnixStream) -> std::io::Result<()> {
    Ok(())
}

// ── Subscribe / Unsubscribe / Cancel / Refresh types ─────────────────────────

/// Error returned when [`DustPlugin::subscribe`] fails.
///
/// Maps to wire error code `-33007 replay_gap`.
#[derive(Debug, Clone)]
pub struct SubscribeError {
    /// Sequence of the oldest event still available; `0` when the plugin has
    /// no ring buffer.
    pub oldest_available: u64,
    /// The `since_sequence` value that triggered the gap.
    pub requested: u64,
}

/// Error returned when [`DustPlugin::unsubscribe`] fails.
#[derive(Debug, Clone)]
pub struct UnsubscribeError {
    /// Human-readable reason.
    pub message: String,
}

/// Acknowledgement returned by a successful [`DustPlugin::cancel`] call.
#[derive(Debug, Clone)]
pub struct CancelAck {
    /// The `op_id` that was targeted.
    pub op_id: String,
    /// `"already_complete"` when the operation finished before the cancel
    /// arrived; `"canceled"` when the operation was actively stopped.
    pub status: String,
}

/// Error returned when [`DustPlugin::cancel`] fails.
#[derive(Debug, Clone)]
pub struct CancelError {
    /// Human-readable reason.
    pub message: String,
}

/// Error returned when [`DustPlugin::refresh`] fails.
#[derive(Debug, Clone)]
pub struct RefreshError {
    /// Human-readable reason.
    pub message: String,
}

// ── DustPlugin trait ─────────────────────────────────────────────────────────

/// Implement this trait to expose a plugin to the Nanika dust dashboard.
///
/// `run()` uses `plugin_id()` to derive the socket path. All other methods
/// are dispatched from incoming `request` envelopes.
///
/// The four optional methods — [`subscribe`][DustPlugin::subscribe],
/// [`unsubscribe`][DustPlugin::unsubscribe], [`cancel`][DustPlugin::cancel],
/// and [`refresh`][DustPlugin::refresh] — have default implementations that
/// represent safe no-op or stub behaviour. Override them to participate in
/// the corresponding protocol flows.
#[async_trait]
pub trait DustPlugin: Send + Sync + 'static {
    /// Stable, URL-safe identifier, e.g. `"dust-tracker"`.
    ///
    /// Determines the socket path: `<runtime_dir>/<plugin-id>.sock`.
    fn plugin_id(&self) -> &str;

    /// Return the plugin manifest (name, version, capabilities, …).
    ///
    /// Dispatched on `request` envelopes with `method: "manifest"` or
    /// `method: "refresh_manifest"`.
    async fn manifest(&self) -> PluginManifest;

    /// Render the plugin's current UI as a list of [`Component`]s.
    ///
    /// Dispatched on a request envelope with `method: "render"`.
    async fn render(&self) -> Vec<Component>;

    /// Execute a user action with the given typed params.
    ///
    /// Dispatched on a request envelope with `method: "action"`.
    async fn action(&self, params: ActionParams) -> ActionResult;

    /// Replay missed events since `since_sequence`.
    ///
    /// Dispatched on a `request` envelope with `method: "events.subscribe"`.
    ///
    /// The default implementation returns [`SubscribeError`] with
    /// `oldest_available: 0`, indicating the plugin keeps no event history.
    /// Override this method to serve replay from an in-memory ring buffer or
    /// persistent store (see [`dust_core::events::EventRing`]).
    async fn subscribe(
        &self,
        since_sequence: u64,
    ) -> Result<Vec<EventEnvelope>, SubscribeError> {
        Err(SubscribeError { oldest_available: 0, requested: since_sequence })
    }

    /// Release an active subscription established by a prior
    /// `events.subscribe`.
    ///
    /// Dispatched on a `request` envelope with `method: "events.unsubscribe"`.
    ///
    /// The default implementation succeeds immediately.
    async fn unsubscribe(&self, _subscription_id: String) -> Result<(), UnsubscribeError> {
        Ok(())
    }

    /// Cancel an in-flight operation identified by `op_id`.
    ///
    /// Dispatched on a `request` envelope with `method: "cancel"`.
    ///
    /// The default implementation returns [`CancelAck`] with
    /// `status: "already_complete"` — treating every cancel as arriving after
    /// completion. Override this method to participate in the
    /// CANCEL-04/05/06 protocol for in-flight operations.
    async fn cancel(&self, op_id: String) -> Result<CancelAck, CancelError> {
        Ok(CancelAck { op_id, status: "already_complete".into() })
    }

    /// Refresh the plugin's internal state on demand.
    ///
    /// Dispatched on a `request` envelope with `method: "refresh"`.
    ///
    /// The default implementation succeeds immediately.
    async fn refresh(&self, _reason: Option<String>) -> Result<(), RefreshError> {
        Ok(())
    }
}

// ── run() ────────────────────────────────────────────────────────────────────

/// Bind the plugin socket at the runtime directory and serve requests until
/// the process exits or an unrecoverable I/O error occurs.
///
/// Socket path: `$XDG_RUNTIME_DIR/nanika/plugins/<plugin-id>.sock` (or
/// `~/.alluka/run/plugins/<plugin-id>.sock` when `XDG_RUNTIME_DIR` is unset).
///
/// Each accepted connection is handled in its own Tokio task. Stale socket
/// files from a previous run are removed automatically.  Connections from
/// foreign UIDs are rejected immediately (error -33004 Unauthorized).
pub async fn run<P: DustPlugin>(plugin: Arc<P>) -> std::io::Result<()> {
    let socket_dir = sdk_runtime_dir()?;
    run_with_dir(plugin, &socket_dir).await
}

/// Like [`run()`] but binds the socket under an explicit `socket_dir`.
///
/// Useful for testing without relying on `$XDG_RUNTIME_DIR` or `$HOME`.
pub async fn run_with_dir<P: DustPlugin>(
    plugin: Arc<P>,
    socket_dir: &Path,
) -> std::io::Result<()> {
    tokio::fs::create_dir_all(socket_dir).await?;

    let socket_path = socket_dir.join(format!("{}.sock", plugin.plugin_id()));

    // Remove stale socket from a previous process run.
    let _ = tokio::fs::remove_file(&socket_path).await;

    let listener = UnixListener::bind(&socket_path)?;

    loop {
        let (stream, _addr) = listener.accept().await?;

        // ── Peer credential check ─────────────────────────────────────────────
        // Reject connections from processes running as a different uid.
        if let Err(e) = check_peer_uid(&stream) {
            eprintln!("dust-sdk: rejecting unauthorized connection: {e}");
            // Drop the stream — no response needed at this level.
            drop(stream);
            continue;
        }

        let plugin = Arc::clone(&plugin);
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, plugin).await {
                // Only log non-EOF errors — EOF is normal when host closes.
                if e.kind() != std::io::ErrorKind::UnexpectedEof
                    && e.kind() != std::io::ErrorKind::TimedOut
                {
                    eprintln!("dust-sdk: connection error: {e}");
                }
            }
        });
    }
}

// ── Wire helpers ──────────────────────────────────────────────────────────────

/// Read one framed envelope from the stream.  Returns `Ok(None)` on clean EOF.
async fn read_envelope_stream(
    stream: &mut UnixStream,
) -> std::io::Result<Option<Envelope>> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(None); // FRAME-04
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    let env: Envelope = serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(env))
}

/// Write one framed envelope to the stream.
async fn write_envelope_stream(
    stream: &mut UnixStream,
    envelope: &Envelope,
) -> std::io::Result<()> {
    let payload = serde_json::to_vec(envelope)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let len = (payload.len() as u32).to_be_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&payload).await?;
    stream.flush().await
}

// ── Connection handler ───────────────────────────────────────────────────────

/// Full per-connection lifecycle for an accepted connection:
///
/// 1. Send `ready` event (manifest + protocol version).
/// 2. Read and validate `host_info` event (5 s timeout).
/// 3. Dispatch loop:
///    - `Request` → dispatch to trait method; write response.
///    - `Heartbeat` → echo back a heartbeat immediately.
///    - `Shutdown` → drain (return cleanly).
///    - `Event` / `Response` → ignored.
async fn handle_connection<P: DustPlugin>(
    mut stream: UnixStream,
    plugin: Arc<P>,
) -> std::io::Result<()> {
    // ── 1. Send ready event ───────────────────────────────────────────────────
    let manifest = plugin.manifest().await;
    let seq = next_seq();
    let ready = Envelope::Event(EventEnvelope {
        id: format!("evt_{seq:016x}"),
        event_type: EventType::Ready,
        ts: utc_now(),
        sequence: Some(seq),
        data: serde_json::json!({
            "manifest": serde_json::to_value(&manifest)
                .unwrap_or(serde_json::Value::Null),
            "protocol_version": SDK_PROTOCOL_VERSION,
            "plugin_info": {
                "pid": std::process::id(),
                "started_at": utc_now()
            }
        }),
    });
    write_envelope_stream(&mut stream, &ready).await?;

    // ── 2. Read and validate host_info (with timeout) ─────────────────────────
    let host_result =
        tokio::time::timeout(HANDSHAKE_TIMEOUT, read_envelope_stream(&mut stream))
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out waiting for host_info",
                )
            })?;

    match host_result? {
        Some(Envelope::Event(ref e)) if e.event_type == EventType::HostInfo => {
            // Host acknowledged — proceed to dispatch loop.
        }
        Some(other) => {
            eprintln!(
                "dust-sdk: expected host_info, got {:?}; closing connection",
                other.kind()
            );
            return Ok(());
        }
        None => {
            return Ok(()); // Connection closed before host_info.
        }
    }

    // ── 3. Dispatch loop ──────────────────────────────────────────────────────
    loop {
        let envelope = match read_envelope_stream(&mut stream).await? {
            Some(e) => e,
            None => return Ok(()), // Clean disconnect.
        };

        match envelope {
            Envelope::Request(req) => {
                let resp = dispatch(&*plugin, req).await;
                write_envelope_stream(&mut stream, &resp).await?;
            }
            Envelope::Heartbeat(_) => {
                // Echo a heartbeat back so the registry's miss-counter resets.
                let hb = Envelope::Heartbeat(HeartbeatEnvelope { ts: utc_now() });
                write_envelope_stream(&mut stream, &hb).await?;
            }
            Envelope::Shutdown(_) => {
                // Registry requested graceful shutdown — drain and return.
                return Ok(());
            }
            // Events from host (e.g. Refresh) and stray Responses are ignored.
            Envelope::Event(_) | Envelope::Response(_) => {}
        }
    }
}

// ── Dispatch ─────────────────────────────────────────────────────────────────

async fn dispatch<P: DustPlugin>(plugin: &P, req: RequestEnvelope) -> Envelope {
    let response = match req.method.as_str() {
        "manifest" => {
            let manifest = plugin.manifest().await;
            ResponseEnvelope {
                id: req.id,
                result: Some(
                    serde_json::to_value(&manifest)
                        .expect("manifest must be serializable"),
                ),
                error: None,
            }
        }
        // refresh_manifest: host requests the plugin re-read and return its
        // manifest (e.g. after plugin.json was modified in-place).
        "refresh_manifest" => {
            let manifest = plugin.manifest().await;
            ResponseEnvelope {
                id: req.id,
                result: Some(
                    serde_json::to_value(&manifest)
                        .expect("manifest must be serializable"),
                ),
                error: None,
            }
        }
        "render" => {
            let components = plugin.render().await;
            ResponseEnvelope {
                id: req.id,
                result: Some(
                    serde_json::to_value(&components)
                        .expect("components must be serializable"),
                ),
                error: None,
            }
        }
        "action" => {
            let params: ActionParams =
                serde_json::from_value(req.params).unwrap_or_default();
            let result = plugin.action(params).await;
            ResponseEnvelope {
                id: req.id,
                result: Some(
                    serde_json::to_value(&result)
                        .expect("ActionResult must be serializable"),
                ),
                error: None,
            }
        }
        // ── events.subscribe ─────────────────────────────────────────────
        "events.subscribe" => {
            let since_sequence = req.params
                .get("since_sequence")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            match plugin.subscribe(since_sequence).await {
                Ok(events) => {
                    // REPLAY-04: result MUST be {subscription_id, events, next_sequence}.
                    let next_sequence = events
                        .iter()
                        .filter_map(|e| e.sequence)
                        .max()
                        .map(|s| s + 1)
                        .unwrap_or(since_sequence.max(1));
                    // Per-request subscription handle. The default SDK dispatch does not
                    // route live push itself — plugins that implement streaming override
                    // `subscribe` and manage their own connection state (see dust-fixture-minimal).
                    let subscription_id = format!("sub_{}", req.id);
                    ResponseEnvelope {
                        id: req.id,
                        result: Some(serde_json::json!({
                            "subscription_id": subscription_id,
                            "events": events,
                            "next_sequence": next_sequence,
                        })),
                        error: None,
                    }
                }
                Err(e) => ResponseEnvelope {
                    id: req.id,
                    result: None,
                    error: Some(ErrorObject {
                        code: -33007,
                        message: format!(
                            "replay_gap: requested {} but oldest available is {}",
                            e.requested, e.oldest_available,
                        ),
                        data: Some(serde_json::json!({
                            "oldest_available": e.oldest_available,
                            "requested": e.requested,
                        })),
                    }),
                },
            }
        }

        // ── events.unsubscribe ────────────────────────────────────────────
        "events.unsubscribe" => {
            let subscription_id = req.params
                .get("subscription_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match plugin.unsubscribe(subscription_id).await {
                Ok(()) => ResponseEnvelope {
                    id: req.id,
                    result: Some(serde_json::json!({"ok": true})),
                    error: None,
                },
                Err(e) => ResponseEnvelope {
                    id: req.id,
                    result: None,
                    error: Some(ErrorObject {
                        code: -32603,
                        message: e.message,
                        data: None,
                    }),
                },
            }
        }

        // ── cancel ────────────────────────────────────────────────────────
        "cancel" => {
            let op_id = req.params
                .get("op_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match plugin.cancel(op_id).await {
                Ok(ack) => ResponseEnvelope {
                    id: req.id,
                    result: Some(serde_json::json!({
                        "ok": true,
                        "already_complete": ack.status == "already_complete",
                        "op_id": ack.op_id,
                    })),
                    error: None,
                },
                Err(e) => ResponseEnvelope {
                    id: req.id,
                    result: None,
                    error: Some(ErrorObject {
                        code: -32603,
                        message: e.message,
                        data: None,
                    }),
                },
            }
        }

        // ── refresh ───────────────────────────────────────────────────────
        "refresh" => {
            let reason = req.params
                .get("reason")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            match plugin.refresh(reason).await {
                Ok(()) => ResponseEnvelope {
                    id: req.id,
                    result: Some(serde_json::json!({})),
                    error: None,
                },
                Err(e) => ResponseEnvelope {
                    id: req.id,
                    result: None,
                    error: Some(ErrorObject {
                        code: -32603,
                        message: e.message,
                        data: None,
                    }),
                },
            }
        }

        method => ResponseEnvelope {
            id: req.id,
            result: None,
            error: Some(ErrorObject {
                code: -32601,
                message: format!("method not found: {method}"),
                data: None,
            }),
        },
    };
    Envelope::Response(response)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use dust_core::Capability;

    struct TestPlugin;

    #[async_trait]
    impl DustPlugin for TestPlugin {
        fn plugin_id(&self) -> &str {
            "test-plugin"
        }

        async fn manifest(&self) -> PluginManifest {
            PluginManifest {
                name: "Test".into(),
                version: "0.1.0".into(),
                description: "A test plugin".into(),
                capabilities: vec![Capability::Widget { refresh_secs: 0 }],
                icon: None,
            }
        }

        async fn render(&self) -> Vec<Component> {
            vec![Component::Text {
                content: "hello".into(),
                style: TextStyle::default(),
            }]
        }

        async fn action(&self, _params: ActionParams) -> ActionResult {
            ActionResult::ok()
        }
    }

    // ── Pure dispatch tests (no socket, no lifecycle) ─────────────────────────

    #[tokio::test]
    async fn dispatch_manifest() {
        let plugin = TestPlugin;
        let req = RequestEnvelope {
            id: "1".into(),
            method: "manifest".into(),
            params: serde_json::Value::Null,
        };
        let env = dispatch(&plugin, req).await;
        if let Envelope::Response(resp) = env {
            assert_eq!(resp.id, "1");
            assert!(resp.result.is_some());
            assert!(resp.error.is_none());
            assert_eq!(resp.result.unwrap()["name"], "Test");
        } else {
            panic!("expected Response envelope");
        }
    }

    #[tokio::test]
    async fn dispatch_refresh_manifest() {
        let plugin = TestPlugin;
        let req = RequestEnvelope {
            id: "rm-1".into(),
            method: "refresh_manifest".into(),
            params: serde_json::Value::Null,
        };
        let env = dispatch(&plugin, req).await;
        if let Envelope::Response(resp) = env {
            assert_eq!(resp.id, "rm-1");
            assert!(resp.result.is_some());
            assert!(resp.error.is_none());
            assert_eq!(resp.result.unwrap()["name"], "Test");
        } else {
            panic!("expected Response envelope");
        }
    }

    #[tokio::test]
    async fn dispatch_render() {
        let plugin = TestPlugin;
        let req = RequestEnvelope {
            id: "2".into(),
            method: "render".into(),
            params: serde_json::Value::Null,
        };
        let env = dispatch(&plugin, req).await;
        if let Envelope::Response(resp) = env {
            assert!(resp.result.is_some());
            let arr = resp.result.unwrap();
            assert!(arr.is_array());
            assert_eq!(arr.as_array().unwrap().len(), 1);
        } else {
            panic!("expected Response envelope");
        }
    }

    #[tokio::test]
    async fn dispatch_action() {
        let plugin = TestPlugin;
        let req = RequestEnvelope {
            id: "3".into(),
            method: "action".into(),
            params: serde_json::json!({"op_id": "op_1", "item_id": "item_1"}),
        };
        let env = dispatch(&plugin, req).await;
        if let Envelope::Response(resp) = env {
            assert!(resp.result.is_some());
            assert!(resp.result.unwrap()["success"].as_bool().unwrap());
        } else {
            panic!("expected Response envelope");
        }
    }

    // ── Dispatch tests for the four optional trait methods ────────────────

    #[tokio::test]
    async fn dispatch_subscribe_default_returns_replay_gap() {
        let plugin = TestPlugin;
        let req = RequestEnvelope {
            id: "sub-1".into(),
            method: "events.subscribe".into(),
            params: serde_json::json!({"since_sequence": 5}),
        };
        let env = dispatch(&plugin, req).await;
        if let Envelope::Response(resp) = env {
            assert!(resp.result.is_none());
            let err = resp.error.expect("default subscribe must return an error");
            assert_eq!(err.code, -33007, "expected replay_gap code");
            let data = err.data.expect("replay_gap must carry data");
            assert_eq!(data["requested"].as_u64(), Some(5));
            assert_eq!(data["oldest_available"].as_u64(), Some(0));
        } else {
            panic!("expected Response envelope");
        }
    }

    #[tokio::test]
    async fn dispatch_subscribe_zero_sequence_default_is_replay_gap() {
        // since_sequence=0 still hits the default → ReplayGap with requested=0
        let plugin = TestPlugin;
        let req = RequestEnvelope {
            id: "sub-zero".into(),
            method: "events.subscribe".into(),
            params: serde_json::json!({"since_sequence": 0}),
        };
        let env = dispatch(&plugin, req).await;
        if let Envelope::Response(resp) = env {
            assert!(resp.error.is_some());
            assert_eq!(resp.error.unwrap().code, -33007);
        } else {
            panic!("expected Response envelope");
        }
    }

    #[tokio::test]
    async fn dispatch_unsubscribe_default_returns_ok() {
        let plugin = TestPlugin;
        let req = RequestEnvelope {
            id: "unsub-1".into(),
            method: "events.unsubscribe".into(),
            params: serde_json::json!({"subscription_id": "sub_abc"}),
        };
        let env = dispatch(&plugin, req).await;
        if let Envelope::Response(resp) = env {
            assert!(resp.error.is_none());
            let result = resp.result.expect("unsubscribe must have result");
            assert_eq!(result["ok"], true);
        } else {
            panic!("expected Response envelope");
        }
    }

    #[tokio::test]
    async fn dispatch_cancel_default_returns_already_complete() {
        let plugin = TestPlugin;
        let req = RequestEnvelope {
            id: "cancel-1".into(),
            method: "cancel".into(),
            params: serde_json::json!({"op_id": "op_abc123"}),
        };
        let env = dispatch(&plugin, req).await;
        if let Envelope::Response(resp) = env {
            assert!(resp.error.is_none());
            let result = resp.result.expect("cancel must have result");
            assert_eq!(result["ok"], true);
            assert_eq!(
                result["already_complete"].as_bool(),
                Some(true),
                "default cancel must report already_complete"
            );
            assert_eq!(result["op_id"].as_str(), Some("op_abc123"));
        } else {
            panic!("expected Response envelope");
        }
    }

    #[tokio::test]
    async fn dispatch_refresh_default_returns_ok() {
        let plugin = TestPlugin;
        let req = RequestEnvelope {
            id: "refresh-1".into(),
            method: "refresh".into(),
            params: serde_json::json!({"reason": "user_request"}),
        };
        let env = dispatch(&plugin, req).await;
        if let Envelope::Response(resp) = env {
            assert!(resp.error.is_none());
            assert!(resp.result.is_some());
        } else {
            panic!("expected Response envelope");
        }
    }

    #[tokio::test]
    async fn dispatch_refresh_no_reason_default_returns_ok() {
        let plugin = TestPlugin;
        let req = RequestEnvelope {
            id: "refresh-2".into(),
            method: "refresh".into(),
            params: serde_json::Value::Null,
        };
        let env = dispatch(&plugin, req).await;
        if let Envelope::Response(resp) = env {
            assert!(resp.error.is_none());
        } else {
            panic!("expected Response envelope");
        }
    }

    #[tokio::test]
    async fn dispatch_unknown_method() {
        let plugin = TestPlugin;
        let req = RequestEnvelope {
            id: "4".into(),
            method: "unknown".into(),
            params: serde_json::Value::Null,
        };
        let env = dispatch(&plugin, req).await;
        if let Envelope::Response(resp) = env {
            assert!(resp.error.is_some());
            assert_eq!(resp.error.unwrap().code, -32601);
        } else {
            panic!("expected Response envelope");
        }
    }

    // ── Full lifecycle roundtrip (with ready/host_info handshake) ─────────────

    #[tokio::test]
    async fn run_bind_and_roundtrip() {
        use std::time::Duration;
        use tokio::net::UnixStream;

        let plugin = Arc::new(TestPlugin);
        let plugin_id = plugin.plugin_id().to_string();

        // Use /tmp/dust for the test socket dir (predictable, no HOME needed).
        let socket_dir = PathBuf::from("/tmp/dust");
        let socket_path = socket_dir.join(format!("{plugin_id}.sock"));

        // Clean up before test.
        let _ = tokio::fs::remove_file(&socket_path).await;

        // Spawn the server in the background.
        let server_plugin = Arc::clone(&plugin);
        let server_dir = socket_dir.clone();
        tokio::spawn(async move {
            run_with_dir(server_plugin, &server_dir).await.ok();
        });

        // Give the listener time to bind.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Connect.
        let mut client = UnixStream::connect(&socket_path).await.unwrap();

        // ── 1. Read the ready event ───────────────────────────────────────────
        let ready_env = read_framed_envelope(&mut client).await;
        if let Envelope::Event(e) = ready_env {
            assert_eq!(e.event_type, EventType::Ready);
            let pv = e.data["protocol_version"].as_str().unwrap_or("");
            assert_eq!(pv, SDK_PROTOCOL_VERSION);
            let name = e.data["manifest"]["name"].as_str().unwrap_or("");
            assert_eq!(name, "Test");
        } else {
            panic!("expected ready event");
        }

        // ── 2. Send host_info ─────────────────────────────────────────────────
        let host_info = Envelope::Event(EventEnvelope {
            id: "evt_host0000000001".into(),
            event_type: EventType::HostInfo,
            ts: utc_now(),
            sequence: None,
            data: serde_json::json!({
                "host_name": "dust",
                "host_version": "0.1.0",
                "protocol_version_supported": {"min": "1.0.0", "max": "1.999.999"},
                "consumer_count": 1
            }),
        });
        write_framed_envelope(&mut client, &host_info).await;

        // ── 3. Send manifest request ──────────────────────────────────────────
        let req_env = Envelope::Request(RequestEnvelope {
            id: "rt-1".into(),
            method: "manifest".into(),
            params: serde_json::Value::Null,
        });
        write_framed_envelope(&mut client, &req_env).await;

        // ── 4. Read manifest response ─────────────────────────────────────────
        let resp_env = read_framed_envelope(&mut client).await;
        if let Envelope::Response(resp) = resp_env {
            assert_eq!(resp.id, "rt-1");
            assert!(resp.result.is_some());
            assert_eq!(resp.result.unwrap()["name"], "Test");
        } else {
            panic!("expected Response envelope");
        }
    }

    /// Test that the server sends back a heartbeat when it receives one.
    #[tokio::test]
    async fn heartbeat_echo() {
        use std::time::Duration;
        use tokio::net::UnixStream;

        let plugin = Arc::new(TestPlugin);
        let socket_dir = PathBuf::from("/tmp/dust");
        let socket_path = socket_dir.join("test-plugin-hb.sock");

        let _ = tokio::fs::remove_file(&socket_path).await;

        let server_dir = socket_dir.clone();
        let server_plugin = Arc::clone(&plugin);

        // Use a different plugin ID for this test via a wrapper.
        struct HbPlugin;
        #[async_trait]
        impl DustPlugin for HbPlugin {
            fn plugin_id(&self) -> &str { "test-plugin-hb" }
            async fn manifest(&self) -> PluginManifest {
                PluginManifest {
                    name: "HbTest".into(),
                    version: "0.1.0".into(),
                    description: "HB test plugin".into(),
                    capabilities: vec![],
                    icon: None,
                }
            }
            async fn render(&self) -> Vec<Component> { vec![] }
            async fn action(&self, _: ActionParams) -> ActionResult { ActionResult::ok() }
        }
        let _ = server_plugin; // suppress unused warning

        let hb_plugin = Arc::new(HbPlugin);
        let server_dir2 = server_dir.clone();
        tokio::spawn(async move {
            run_with_dir(hb_plugin, &server_dir2).await.ok();
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut client = UnixStream::connect(&socket_path).await.unwrap();

        // Complete handshake.
        let _ = read_framed_envelope(&mut client).await; // ready
        let host_info = Envelope::Event(EventEnvelope {
            id: "evt_hbhost0000000001".into(),
            event_type: EventType::HostInfo,
            ts: utc_now(),
            sequence: None,
            data: serde_json::json!({"host_name": "dust", "host_version": "0.1.0",
                "protocol_version_supported": {"min": "1.0.0", "max": "1.999.999"},
                "consumer_count": 1}),
        });
        write_framed_envelope(&mut client, &host_info).await;

        // Send a heartbeat.
        let hb = Envelope::Heartbeat(HeartbeatEnvelope { ts: utc_now() });
        write_framed_envelope(&mut client, &hb).await;

        // Expect an echoed heartbeat back.
        let echo = read_framed_envelope(&mut client).await;
        assert!(
            matches!(echo, Envelope::Heartbeat(_)),
            "expected echoed heartbeat, got {echo:?}"
        );
    }

    // ── Test helpers ──────────────────────────────────────────────────────────

    async fn read_framed_envelope(stream: &mut UnixStream) -> Envelope {
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await.unwrap();
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf).await.unwrap();
        serde_json::from_slice(&buf).expect("valid envelope JSON")
    }

    async fn write_framed_envelope(stream: &mut UnixStream, env: &Envelope) {
        let payload = serde_json::to_vec(env).unwrap();
        let len = (payload.len() as u32).to_be_bytes();
        stream.write_all(&len).await.unwrap();
        stream.write_all(&payload).await.unwrap();
        stream.flush().await.unwrap();
    }
}
