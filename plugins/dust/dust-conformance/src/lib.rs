//! dust-conformance — protocol conformance runner for dust plugins.
//!
//! Spawns a plugin binary from its `plugin.json` manifest, waits for the Unix
//! socket to appear, connects, and provides `send_frame`/`recv_frame` helpers
//! for driving protocol scenarios.
//!
//! # Sections
//!
//! | Section | What it checks |
//! |---------|----------------|
//! | `handshake` | Plugin sends a well-formed `ready` event and accepts `host_info` |
//! | `methods`   | `manifest`, `render`, and `action` responses are well-formed |
//! | `heartbeat` | Plugin echoes a heartbeat within the receive timeout |
//! | `shutdown`  | Plugin closes the connection after receiving `shutdown` |
//! | `replay`    | Plugin answers `events.subscribe{since_sequence:0}` with a well-formed `{subscription_id, events, next_sequence}` result (spec §10 / REPLAY-04) |

use std::path::{Path, PathBuf};
use std::time::Duration;

use dust_core::envelope::{
    Envelope, EventEnvelope, EventType, HeartbeatEnvelope, RequestEnvelope, ResponseEnvelope,
    ShutdownEnvelope, ShutdownReason,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum frame size accepted during conformance testing (1 MiB, mirrors dust-core).
const MAX_FRAME_SIZE: usize = 1 << 20;

/// Default per-frame receive timeout used in section checks.
pub const RECV_TIMEOUT: Duration = Duration::from_secs(3);

// ── Error ─────────────────────────────────────────────────────────────────────

/// Errors that can occur in the conformance runner.
#[derive(Debug)]
pub enum ConformError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Timeout(&'static str),
    BadManifest(String),
    Protocol(String),
}

impl std::fmt::Display for ConformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Json(e) => write!(f, "JSON error: {e}"),
            Self::Timeout(msg) => write!(f, "timeout: {msg}"),
            Self::BadManifest(msg) => write!(f, "bad manifest: {msg}"),
            Self::Protocol(msg) => write!(f, "protocol error: {msg}"),
        }
    }
}

impl std::error::Error for ConformError {}

impl From<std::io::Error> for ConformError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for ConformError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

// ── ConformanceResult ─────────────────────────────────────────────────────────

/// Outcome of a single conformance section.
#[derive(Debug)]
pub struct ConformanceResult {
    pub section: String,
    pub passed: bool,
    pub message: String,
}

// ── ConformanceRunner ─────────────────────────────────────────────────────────

/// Spawns a dust plugin from its `plugin.json` manifest, waits for the socket,
/// connects, and provides `send_frame`/`recv_frame` helpers.
///
/// ## Socket isolation
///
/// A per-invocation temporary directory is created under `/tmp/dust-conform-<pid>-<ms>`.
/// `XDG_RUNTIME_DIR` is set to that directory when spawning the child so the
/// plugin binds its socket at `<workdir>/nanika/plugins/<plugin-id>.sock`.
/// The runner polls that directory for `.sock` files until `spawn_timeout_ms`
/// (from `plugin.json`) elapses.
///
/// On drop, the child is killed (best-effort) and the work directory is removed.
pub struct ConformanceRunner {
    child: tokio::process::Child,
    stream: UnixStream,
    /// Absolute path of the `.sock` file the plugin created.
    pub socket_path: PathBuf,
    work_dir: PathBuf,
}

impl ConformanceRunner {
    /// Spawn the plugin described by `manifest_path` (a `plugin.json` file).
    ///
    /// Reads `dust.binary` (relative to the manifest directory), spawns the
    /// binary with `XDG_RUNTIME_DIR` pointed at a fresh temp directory, polls
    /// for a `.sock` file, then connects and returns.
    pub async fn spawn(manifest_path: &Path) -> Result<Self, ConformError> {
        // ── Parse plugin.json ─────────────────────────────────────────────────
        let raw = tokio::fs::read_to_string(manifest_path).await?;
        let manifest_value: serde_json::Value = serde_json::from_str(&raw)?;

        let dust = manifest_value
            .get("dust")
            .ok_or_else(|| ConformError::BadManifest("missing `dust` block".into()))?;

        let binary = dust
            .get("binary")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ConformError::BadManifest("missing `dust.binary`".into()))?;

        let spawn_timeout_ms = dust
            .get("spawn_timeout_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(5_000);

        // Optional extra arguments for multi-command CLIs (GAP-01).
        let extra_args: Vec<String> = dust
            .get("args")
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        // ── Resolve binary path ───────────────────────────────────────────────
        let binary_path = if Path::new(binary).is_absolute() {
            PathBuf::from(binary)
        } else {
            manifest_path
                .parent()
                .ok_or_else(|| {
                    ConformError::BadManifest("manifest path has no parent directory".into())
                })?
                .join(binary)
        };

        // ── Create isolated work directory ────────────────────────────────────
        let pid = std::process::id();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let work_dir = PathBuf::from(format!("/tmp/dust-conform-{pid}-{ts}"));
        let socket_dir = work_dir.join("nanika").join("plugins");
        tokio::fs::create_dir_all(&socket_dir).await?;

        // ── Spawn the plugin ──────────────────────────────────────────────────
        let child = tokio::process::Command::new(&binary_path)
            .args(&extra_args)
            .env("XDG_RUNTIME_DIR", &work_dir)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                ConformError::Io(std::io::Error::new(
                    e.kind(),
                    format!("failed to spawn {:?}: {e}", binary_path),
                ))
            })?;

        // ── Wait for socket ───────────────────────────────────────────────────
        let socket_path =
            wait_for_socket(&socket_dir, Duration::from_millis(spawn_timeout_ms))
                .await
                .ok_or(ConformError::Timeout(
                    "plugin socket did not appear within spawn_timeout_ms",
                ))?;

        // ── Connect ───────────────────────────────────────────────────────────
        let stream = UnixStream::connect(&socket_path).await?;

        Ok(Self { child, stream, socket_path, work_dir })
    }

    /// Spawn a plugin binary directly (no `plugin.json` required).
    ///
    /// Useful in integration tests where the binary path is known at compile time.
    /// An isolated `XDG_RUNTIME_DIR` is created the same way as [`spawn`].
    pub async fn spawn_binary(
        binary_path: &std::path::Path,
        spawn_timeout: Duration,
    ) -> Result<Self, ConformError> {
        Self::spawn_binary_with_envs(binary_path, spawn_timeout, &[]).await
    }

    /// Spawn a plugin binary with additional environment variables.
    ///
    /// Identical to [`spawn_binary`] but injects extra `(key, value)` pairs
    /// into the child environment.  Useful for parameterising fixture behaviour
    /// (e.g., `DUST_HEARTBEAT_INTERVAL_MS`) from individual test cases.
    pub async fn spawn_binary_with_envs(
        binary_path: &std::path::Path,
        spawn_timeout: Duration,
        extra_envs: &[(String, String)],
    ) -> Result<Self, ConformError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let idx = COUNTER.fetch_add(1, Ordering::Relaxed);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let work_dir = PathBuf::from(format!("/tmp/dust-conform-{pid}-{idx}-{ts}"));
        let socket_dir = work_dir.join("nanika").join("plugins");
        tokio::fs::create_dir_all(&socket_dir).await?;

        let mut cmd = tokio::process::Command::new(binary_path);
        cmd.env("XDG_RUNTIME_DIR", &work_dir).kill_on_drop(true);
        for (k, v) in extra_envs {
            cmd.env(k, v);
        }
        let child = cmd.spawn().map_err(|e| {
            ConformError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to spawn {:?}: {e}", binary_path),
            ))
        })?;

        let socket_path = wait_for_socket(&socket_dir, spawn_timeout)
            .await
            .ok_or(ConformError::Timeout(
                "plugin socket did not appear within spawn_timeout",
            ))?;

        let stream = UnixStream::connect(&socket_path).await?;
        Ok(Self { child, stream, socket_path, work_dir })
    }

    /// Write one length-prefixed envelope to the plugin.
    pub async fn send_frame(&mut self, env: &Envelope) -> std::io::Result<()> {
        let payload = serde_json::to_vec(env)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let len = (payload.len() as u32).to_be_bytes();
        self.stream.write_all(&len).await?;
        self.stream.write_all(&payload).await?;
        self.stream.flush().await
    }

    /// Write raw bytes to the socket without any envelope serialization.
    ///
    /// Useful for framing conformance tests that need to inject malformed data
    /// (oversized length prefixes, invalid UTF-8 payloads, etc.).
    pub async fn send_raw_bytes(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(data).await?;
        self.stream.flush().await
    }

    /// Build and write a length-prefixed frame whose payload is arbitrary bytes.
    ///
    /// The 4-byte big-endian length prefix is computed from `payload.len()`.
    /// Useful for sending frames whose content is valid in length but invalid
    /// in content (bad UTF-8, non-object JSON, missing `kind`, etc.).
    pub async fn send_raw_frame(&mut self, payload: &[u8]) -> std::io::Result<()> {
        let len = (payload.len() as u32).to_be_bytes();
        self.stream.write_all(&len).await?;
        self.stream.write_all(payload).await?;
        self.stream.flush().await
    }

    /// Drive the ready/host_info handshake.
    ///
    /// Reads the `ready` event, validates its fields, then sends `host_info`.
    /// Returns `ready.data` on success.
    pub async fn do_handshake(&mut self) -> Result<serde_json::Value, String> {
        complete_handshake(self).await
    }

    /// Read one length-prefixed envelope from the plugin.
    ///
    /// Returns `Ok(None)` on clean EOF.
    pub async fn recv_frame(&mut self) -> std::io::Result<Option<Envelope>> {
        let mut len_buf = [0u8; 4];
        match self.stream.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let len = u32::from_be_bytes(len_buf) as usize;
        if len == 0 {
            return Ok(None);
        }
        if len > MAX_FRAME_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("frame size {len} exceeds MAX_FRAME_SIZE ({MAX_FRAME_SIZE})"),
            ));
        }
        let mut buf = vec![0u8; len];
        self.stream.read_exact(&mut buf).await?;
        serde_json::from_slice(&buf)
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Kill the child process (best-effort).
    pub async fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill().await
    }

    /// Wait for the child process to exit.
    pub async fn wait_exit(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }
}

impl Drop for ConformanceRunner {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_dir_all(&self.work_dir);
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Poll `socket_dir` for a `.sock` file, returning its path when found.
/// Returns `None` if `timeout` elapses without a socket appearing.
async fn wait_for_socket(socket_dir: &Path, timeout: Duration) -> Option<PathBuf> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        if let Ok(mut rd) = tokio::fs::read_dir(socket_dir).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("sock") {
                    return Some(path);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// ISO 8601 timestamp with millisecond precision, without external dependencies.
pub fn utc_now() -> String {
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

/// Build the `host_info` event that the conformance runner sends during handshake.
fn make_host_info() -> Envelope {
    Envelope::Event(EventEnvelope {
        id: "evt_0000000000000001".into(),
        event_type: EventType::HostInfo,
        ts: utc_now(),
        sequence: None,
        data: serde_json::json!({
            "host_name": "dust-conform",
            "host_version": "0.1.0",
            "protocol_version_supported": {
                "min": "1.0.0",
                "max": "1.999.999"
            },
            "consumer_count": 1
        }),
    })
}

// ── Handshake helper ──────────────────────────────────────────────────────────

/// Drive the ready/host_info handshake.
///
/// Reads one frame (expected: `ready` event), validates required fields, then
/// sends `host_info`.  Returns the `ready.data` payload on success.
async fn complete_handshake(runner: &mut ConformanceRunner) -> Result<serde_json::Value, String> {
    let frame = tokio::time::timeout(RECV_TIMEOUT, runner.recv_frame())
        .await
        .map_err(|_| "timed out waiting for ready event".to_string())?
        .map_err(|e| format!("I/O error reading ready event: {e}"))?;

    let ready_data = match frame {
        Some(Envelope::Event(e)) if e.event_type == EventType::Ready => e.data,
        Some(other) => {
            return Err(format!(
                "expected ready event, got {:?}",
                other.kind()
            ))
        }
        None => return Err("plugin closed connection before sending ready".into()),
    };

    runner
        .send_frame(&make_host_info())
        .await
        .map_err(|e| format!("I/O error sending host_info: {e}"))?;

    Ok(ready_data)
}

// ── Section dispatch ──────────────────────────────────────────────────────────

/// Run a named conformance section against the plugin at `manifest_path`.
///
/// Spawns a fresh process for each call.
pub async fn run_section(manifest_path: &Path, section: &str) -> ConformanceResult {
    let result = do_run_section(manifest_path, section).await;
    ConformanceResult {
        section: section.to_string(),
        passed: result.is_ok(),
        message: result.unwrap_or_else(|e| e),
    }
}

async fn do_run_section(manifest_path: &Path, section: &str) -> Result<String, String> {
    match section {
        "handshake" => check_handshake(manifest_path).await,
        "methods" => check_methods(manifest_path).await,
        "heartbeat" => check_heartbeat(manifest_path).await,
        "shutdown" => check_shutdown(manifest_path).await,
        "replay" => check_replay(manifest_path).await,
        other => Err(format!("unknown section: `{other}`")),
    }
}

// ── Section: handshake ────────────────────────────────────────────────────────

/// Verify the plugin sends a well-formed `ready` event (HANDSHAKE-01).
async fn check_handshake(manifest_path: &Path) -> Result<String, String> {
    let mut runner = ConformanceRunner::spawn(manifest_path)
        .await
        .map_err(|e| format!("spawn failed: {e}"))?;

    let ready_data = complete_handshake(&mut runner).await?;

    let protocol_version = ready_data
        .get("protocol_version")
        .and_then(serde_json::Value::as_str)
        .ok_or("ready.data missing `protocol_version`")?;

    if protocol_version.is_empty() {
        return Err("ready.protocol_version is empty".into());
    }

    let manifest = ready_data
        .get("manifest")
        .ok_or("ready.data missing `manifest`")?;

    manifest
        .get("name")
        .and_then(serde_json::Value::as_str)
        .ok_or("ready.manifest missing `name`")?;

    manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or("ready.manifest missing `version`")?;

    Ok(format!("ready received (protocol_version={protocol_version})"))
}

// ── Section: methods ──────────────────────────────────────────────────────────

/// Verify `manifest`, `render`, and `action` responses are well-formed.
async fn check_methods(manifest_path: &Path) -> Result<String, String> {
    let mut runner = ConformanceRunner::spawn(manifest_path)
        .await
        .map_err(|e| format!("spawn failed: {e}"))?;

    complete_handshake(&mut runner).await?;

    // manifest
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "req_0000000000000001".into(),
            method: "manifest".into(),
            params: serde_json::Value::Null,
        }))
        .await
        .map_err(|e| format!("send manifest request: {e}"))?;

    let resp = recv_response(&mut runner, "req_0000000000000001").await?;
    let result = resp.result.ok_or("manifest response missing result")?;
    result
        .get("name")
        .and_then(serde_json::Value::as_str)
        .ok_or("manifest.result missing `name`")?;
    result
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or("manifest.result missing `version`")?;

    // render
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "req_0000000000000002".into(),
            method: "render".into(),
            params: serde_json::Value::Null,
        }))
        .await
        .map_err(|e| format!("send render request: {e}"))?;

    let resp = recv_response(&mut runner, "req_0000000000000002").await?;
    let result = resp.result.ok_or("render response missing result")?;
    if !result.is_array() {
        return Err(format!("render result must be a JSON array, got: {result}"));
    }

    // action
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "req_0000000000000003".into(),
            method: "action".into(),
            params: serde_json::json!({}),
        }))
        .await
        .map_err(|e| format!("send action request: {e}"))?;

    let resp = recv_response(&mut runner, "req_0000000000000003").await?;
    let result = resp.result.ok_or("action response missing result")?;
    result
        .get("success")
        .and_then(serde_json::Value::as_bool)
        .ok_or("action.result missing `success` boolean")?;

    Ok("manifest/render/action responses are well-formed".into())
}

// ── Section: heartbeat ────────────────────────────────────────────────────────

/// Verify the plugin echoes a heartbeat.
async fn check_heartbeat(manifest_path: &Path) -> Result<String, String> {
    let mut runner = ConformanceRunner::spawn(manifest_path)
        .await
        .map_err(|e| format!("spawn failed: {e}"))?;

    complete_handshake(&mut runner).await?;

    runner
        .send_frame(&Envelope::Heartbeat(HeartbeatEnvelope { ts: utc_now() }))
        .await
        .map_err(|e| format!("send heartbeat: {e}"))?;

    let frame = tokio::time::timeout(RECV_TIMEOUT, runner.recv_frame())
        .await
        .map_err(|_| "timed out waiting for heartbeat echo".to_string())?
        .map_err(|e| format!("I/O error reading heartbeat echo: {e}"))?;

    match frame {
        Some(Envelope::Heartbeat(_)) => Ok("heartbeat echo received".into()),
        Some(other) => Err(format!(
            "expected heartbeat echo, got {:?}",
            other.kind()
        )),
        None => Err("plugin closed connection without echoing heartbeat".into()),
    }
}

// ── Section: shutdown ─────────────────────────────────────────────────────────

/// Verify the plugin closes the connection after receiving `shutdown`.
async fn check_shutdown(manifest_path: &Path) -> Result<String, String> {
    let mut runner = ConformanceRunner::spawn(manifest_path)
        .await
        .map_err(|e| format!("spawn failed: {e}"))?;

    complete_handshake(&mut runner).await?;

    runner
        .send_frame(&Envelope::Shutdown(ShutdownEnvelope {
            reason: ShutdownReason::HostExit,
        }))
        .await
        .map_err(|e| format!("send shutdown: {e}"))?;

    let frame = tokio::time::timeout(RECV_TIMEOUT, runner.recv_frame())
        .await
        .map_err(|_| "timed out waiting for plugin to close after shutdown".to_string())?
        .map_err(|e| format!("I/O error reading post-shutdown frame: {e}"))?;

    match frame {
        None => Ok("plugin closed connection after shutdown".into()),
        Some(other) => {
            // Some plugins may emit a final event before closing — accept that.
            eprintln!(
                "dust-conform: got {:?} after shutdown; waiting for close",
                other.kind()
            );
            let frame2 = tokio::time::timeout(RECV_TIMEOUT, runner.recv_frame())
                .await
                .map_err(|_| "timed out waiting for close after final event".to_string())?
                .map_err(|e| format!("I/O error on second post-shutdown read: {e}"))?;
            match frame2 {
                None => Ok("plugin closed after final event".into()),
                Some(_) => Err("plugin did not close connection after shutdown".into()),
            }
        }
    }
}

// ── Section: replay ───────────────────────────────────────────────────────────

/// Verify the plugin answers `events.subscribe{since_sequence:0}` with a
/// well-formed snapshot (REPLAY-04, spec §10).
///
/// The result MUST contain:
///   - `subscription_id` (string)
///   - `events` (array)
///   - `next_sequence` (u64)
///
/// Plugins without ring-buffer support (e.g., the language stubs) will return
/// `-33007 replay_gap`; that surfaces as a section failure, which is documented
/// as expected behaviour in DUST-WIRE-SPEC.md §19.
async fn check_replay(manifest_path: &Path) -> Result<String, String> {
    let mut runner = ConformanceRunner::spawn(manifest_path)
        .await
        .map_err(|e| format!("spawn failed: {e}"))?;

    complete_handshake(&mut runner).await?;

    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "req_replay_0000000001".into(),
            method: "events.subscribe".into(),
            params: serde_json::json!({ "since_sequence": 0 }),
        }))
        .await
        .map_err(|e| format!("send events.subscribe request: {e}"))?;

    let resp = recv_response(&mut runner, "req_replay_0000000001").await?;
    let result = resp
        .result
        .ok_or("events.subscribe response missing `result`")?;

    let subscription_id = result
        .get("subscription_id")
        .and_then(serde_json::Value::as_str)
        .ok_or("REPLAY-04: result missing string `subscription_id`")?;

    let events = result
        .get("events")
        .and_then(serde_json::Value::as_array)
        .ok_or("REPLAY-04: result missing array `events`")?;

    let next_sequence = result
        .get("next_sequence")
        .and_then(serde_json::Value::as_u64)
        .ok_or("REPLAY-04: result missing u64 `next_sequence`")?;

    Ok(format!(
        "events.subscribe result well-formed (subscription_id={subscription_id}, events={}, next_sequence={next_sequence})",
        events.len()
    ))
}

// ── recv_response helper ──────────────────────────────────────────────────────

/// Receive a response envelope for the given request ID.
///
/// Heartbeat frames that arrive while waiting are silently skipped — the
/// fixture may send proactive heartbeats at any time during `active` state
/// (HEARTBEAT-01) and callers should not need to handle them.
///
/// Returns `Err` if the response contains an `error` field.
/// Exposed so integration tests can send requests and assert on the response.
pub async fn recv_response(
    runner: &mut ConformanceRunner,
    expected_id: &str,
) -> Result<ResponseEnvelope, String> {
    loop {
        let frame = tokio::time::timeout(RECV_TIMEOUT, runner.recv_frame())
            .await
            .map_err(|_| format!("timed out waiting for response to {expected_id}"))?
            .map_err(|e| format!("I/O error reading response: {e}"))?;

        match frame {
            // Proactive heartbeats from the fixture are transparent.
            Some(Envelope::Heartbeat(_)) => continue,

            Some(Envelope::Response(resp)) => {
                if resp.id != expected_id {
                    return Err(format!(
                        "response id mismatch: expected {expected_id}, got {}",
                        resp.id
                    ));
                }
                if let Some(ref err) = resp.error {
                    return Err(format!(
                        "method `{}` returned error {}: {}",
                        expected_id, err.code, err.message
                    ));
                }
                return Ok(resp);
            }
            Some(other) => {
                return Err(format!(
                    "expected response for {expected_id}, got {:?}",
                    other.kind()
                ))
            }
            None => {
                return Err(format!(
                    "plugin closed connection waiting for response to {expected_id}"
                ))
            }
        }
    }
}

/// Receive the raw `ResponseEnvelope` for any request ID, including error responses.
///
/// Unlike [`recv_response`], this does NOT fail if the response has an `error` field.
/// Heartbeat frames are skipped transparently (see [`recv_response`]).
/// Used in tests that assert on specific error codes.
pub async fn recv_raw_response(
    runner: &mut ConformanceRunner,
) -> Result<ResponseEnvelope, String> {
    loop {
        let frame = tokio::time::timeout(RECV_TIMEOUT, runner.recv_frame())
            .await
            .map_err(|_| "timed out waiting for response".to_string())?
            .map_err(|e| format!("I/O error reading response: {e}"))?;

        match frame {
            Some(Envelope::Heartbeat(_)) => continue,
            Some(Envelope::Response(resp)) => return Ok(resp),
            Some(other) => return Err(format!("expected response, got {:?}", other.kind())),
            None => return Err("plugin closed connection waiting for response".into()),
        }
    }
}

/// Receive the next `EventEnvelope`, skipping heartbeats.
///
/// Used in live-push tests (REPLAY-06) where the fixture pushes a `data_updated`
/// event after processing an action while a subscription is active.
pub async fn recv_next_event(
    runner: &mut ConformanceRunner,
) -> Result<EventEnvelope, String> {
    loop {
        let frame = tokio::time::timeout(RECV_TIMEOUT, runner.recv_frame())
            .await
            .map_err(|_| "timed out waiting for pushed event".to_string())?
            .map_err(|e| format!("I/O error reading event: {e}"))?;

        match frame {
            Some(Envelope::Heartbeat(_)) => continue,
            Some(Envelope::Event(e)) => return Ok(e),
            Some(other) => {
                return Err(format!(
                    "expected event (live push), got {:?}",
                    other.kind()
                ))
            }
            None => {
                return Err(
                    "plugin closed connection while waiting for pushed event".into(),
                )
            }
        }
    }
}
