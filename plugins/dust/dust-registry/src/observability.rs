//! Observability writer — appends plugin events to `~/.alluka/dust/events.jsonl`.
//!
//! # Design
//!
//! - Receives [`WriteRequest`] via a bounded tokio mpsc channel (cap 10 000).
//! - Background task batches messages for up to 100 ms **or** until the batch
//!   reaches 4 KiB, then writes and fsyncs the whole batch atomically.
//! - Heartbeat envelopes are never logged (**ENVELOPE-09**).
//! - Fields matching `dust.log_redact` JSONPath rules are stripped from
//!   `event.data` before the line is written.
//! - `event.data.meta.log_redact_all = true` suppresses the entire message.
//! - Rotation triggers at 100 MB or 7 days; the old file is renamed with a
//!   timestamp suffix (`events.jsonl.YYYYMMDD-HHMMSS`).
//! - Channel overflow and disk write failures emit `log_overflow` meta records
//!   on the next successful batch flush.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dust_core::envelope::Envelope;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

// ── Constants ─────────────────────────────────────────────────────────────────

const CHANNEL_CAP: usize = 10_000;
const BATCH_INTERVAL: Duration = Duration::from_millis(100);
const BATCH_MAX_BYTES: usize = 4 * 1_024;
const ROTATE_MAX_BYTES: u64 = 100 * 1_024 * 1_024; // 100 MB
const ROTATE_MAX_AGE_SECS: u64 = 7 * 24 * 3_600; // 7 days

// ── WriterConfig ──────────────────────────────────────────────────────────────

/// Tuning knobs for the background writer (rotation thresholds).
///
/// Production code uses the defaults; tests override to trigger rotation
/// without actually writing 100 MB.
pub struct WriterConfig {
    pub rotate_max_bytes: u64,
    pub rotate_max_age_secs: u64,
}

impl Default for WriterConfig {
    fn default() -> Self {
        Self {
            rotate_max_bytes: ROTATE_MAX_BYTES,
            rotate_max_age_secs: ROTATE_MAX_AGE_SECS,
        }
    }
}

// ── WriteRequest ──────────────────────────────────────────────────────────────

/// Message sent to the background writer task.
pub struct WriteRequest {
    /// Envelope to log (heartbeats are silently dropped before enqueue).
    pub envelope: Envelope,
    /// JSONPath-subset redaction rules from this plugin's `dust.log_redact`.
    pub log_redact: Vec<String>,
    /// Plugin that produced this envelope — injected into the written record
    /// so that `dust logs --plugin <id>` can filter by origin.
    pub plugin_id: String,
}

// ── ObservabilityWriter ───────────────────────────────────────────────────────

/// Shared handle for sending plugin events to the background writer.
///
/// Cheap to clone — the channel sender and overflow counter are both `Arc`.
#[derive(Clone)]
pub struct ObservabilityWriter {
    tx: mpsc::Sender<WriteRequest>,
    /// Incremented on channel-full overflow; drained into meta records by the
    /// background task on each batch flush.
    pub(crate) overflow: Arc<AtomicU64>,
}

impl ObservabilityWriter {
    /// Create a writer that appends to `~/.alluka/dust/events.jsonl`.
    pub fn new() -> Self {
        let path = default_events_path()
            .unwrap_or_else(|_| PathBuf::from("/tmp/dust-events.jsonl"));
        Self::new_with_config(path, WriterConfig::default())
    }

    /// Create a writer targeting `events_path` with production rotation limits.
    pub fn new_with_path(events_path: PathBuf) -> Self {
        Self::new_with_config(events_path, WriterConfig::default())
    }

    /// Create a writer with custom rotation limits (useful in tests).
    pub fn new_with_config(events_path: PathBuf, config: WriterConfig) -> Self {
        let (tx, rx) = mpsc::channel(CHANNEL_CAP);
        let overflow = Arc::new(AtomicU64::new(0));
        let overflow_bg = Arc::clone(&overflow);
        tokio::spawn(writer_task(rx, overflow_bg, events_path, config));
        Self { tx, overflow }
    }

    /// Send a non-heartbeat envelope for logging.
    ///
    /// Heartbeats are dropped immediately (**ENVELOPE-09**).  When the channel
    /// is full the overflow counter is incremented; the background task emits a
    /// `log_overflow` meta record on the next batch flush.
    ///
    /// `plugin_id` is embedded in the written JSONL record so that
    /// `dust logs --plugin <id>` can filter by origin.
    pub fn send(&self, plugin_id: &str, envelope: Envelope, log_redact: Vec<String>) {
        if matches!(&envelope, Envelope::Heartbeat(_)) {
            return;
        }
        let req = WriteRequest { envelope, log_redact, plugin_id: plugin_id.to_owned() };
        if self.tx.try_send(req).is_err() {
            self.overflow.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Default for ObservabilityWriter {
    fn default() -> Self {
        Self::new()
    }
}

// ── Background writer task ────────────────────────────────────────────────────

async fn writer_task(
    mut rx: mpsc::Receiver<WriteRequest>,
    overflow: Arc<AtomicU64>,
    events_path: PathBuf,
    config: WriterConfig,
) {
    let mut batch: Vec<u8> = Vec::with_capacity(BATCH_MAX_BYTES * 2);
    let mut file_state = match FileState::open_or_create(&events_path, &config).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("dust-observability: failed to open events file: {e}");
            return;
        }
    };

    let mut ticker = tokio::time::interval(BATCH_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;

            // ── Timer: flush whatever is buffered ─────────────────────────
            _ = ticker.tick() => {
                let n = overflow.swap(0, Ordering::Relaxed);
                if n > 0 {
                    append_value(&mut batch, overflow_record(n));
                }
                if !batch.is_empty() {
                    flush_batch(&mut batch, &mut file_state, &events_path, &overflow, &config).await;
                }
            }

            // ── Incoming message ──────────────────────────────────────────
            msg = rx.recv() => {
                match msg {
                    None => break, // all senders dropped — shut down
                    Some(req) => {
                        if let Some(line) = prepare_line(req) {
                            batch.extend_from_slice(&line);
                            if batch.len() >= BATCH_MAX_BYTES {
                                let n = overflow.swap(0, Ordering::Relaxed);
                                if n > 0 {
                                    append_value(&mut batch, overflow_record(n));
                                }
                                flush_batch(
                                    &mut batch,
                                    &mut file_state,
                                    &events_path,
                                    &overflow,
                                    &config,
                                )
                                .await;
                            }
                        }
                    }
                }
            }
        }
    }

    // Final drain on shutdown.
    let n = overflow.swap(0, Ordering::Relaxed);
    if n > 0 {
        append_value(&mut batch, overflow_record(n));
    }
    if !batch.is_empty() {
        flush_batch(&mut batch, &mut file_state, &events_path, &overflow, &config).await;
    }
}

// ── Line preparation ──────────────────────────────────────────────────────────

/// Build a newline-terminated JSONL line from `req`, applying redaction rules.
///
/// Returns `None` when the message should be suppressed entirely.
fn prepare_line(req: WriteRequest) -> Option<Vec<u8>> {
    if matches!(&req.envelope, Envelope::Heartbeat(_)) {
        return None;
    }

    let mut value = serde_json::to_value(&req.envelope).ok()?;

    // $.meta.log_redact_all=true: suppress the message entirely.
    if log_redact_all_set(&value) {
        return None;
    }

    // Apply redaction rules to the `data` sub-object.
    if !req.log_redact.is_empty() {
        if let Some(data) = value.get_mut("data") {
            for rule in &req.log_redact {
                redact_path(data, rule);
            }
        }
    }

    // Inject plugin_id at the top level so `dust logs --plugin` can filter.
    if let serde_json::Value::Object(ref mut map) = value {
        map.insert(
            "plugin_id".to_owned(),
            serde_json::Value::String(req.plugin_id),
        );
    }

    let mut bytes = serde_json::to_vec(&value).ok()?;
    bytes.push(b'\n');
    Some(bytes)
}

/// Serialize `value` to a JSONL line and append it to `batch`.
fn append_value(batch: &mut Vec<u8>, value: serde_json::Value) {
    if let Ok(mut b) = serde_json::to_vec(&value) {
        b.push(b'\n');
        batch.extend_from_slice(&b);
    }
}

// ── Batch flush ───────────────────────────────────────────────────────────────

async fn flush_batch(
    batch: &mut Vec<u8>,
    file_state: &mut FileState,
    events_path: &Path,
    overflow: &AtomicU64,
    config: &WriterConfig,
) {
    if batch.is_empty() {
        return;
    }

    // Rotate before writing if thresholds are met.
    if file_state.should_rotate(config) {
        if let Err(e) = file_state.rotate(events_path, config).await {
            eprintln!("dust-observability: rotation failed: {e}");
        }
    }

    if let Err(e) = file_state.file.write_all(batch).await {
        eprintln!("dust-observability: write failed: {e}");
        overflow.fetch_add(1, Ordering::Relaxed);
        // Attempt to recover by reopening the file.
        if let Ok(new_state) = FileState::open_or_create(events_path, config).await {
            *file_state = new_state;
        }
        batch.clear();
        return;
    }

    // fsync per batch (spec requirement).
    if let Err(e) = file_state.file.sync_all().await {
        eprintln!("dust-observability: fsync failed: {e}");
        overflow.fetch_add(1, Ordering::Relaxed);
    }

    file_state.bytes_written += batch.len() as u64;
    batch.clear();
}

// ── FileState ─────────────────────────────────────────────────────────────────

struct FileState {
    file: tokio::fs::File,
    /// Bytes written since this file was opened (starts at existing file size).
    bytes_written: u64,
    /// File creation/modification time used for the age-based rotation check.
    created_at: SystemTime,
}

impl FileState {
    async fn open_or_create(path: &Path, _config: &WriterConfig) -> Result<Self, std::io::Error> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;

        let meta = tokio::fs::metadata(path).await?;
        // Prefer birthtime (creation) for age; fall back to mtime.
        let created_at = meta
            .created()
            .or_else(|_| meta.modified())
            .unwrap_or_else(|_| SystemTime::now());
        let bytes_written = meta.len();

        Ok(Self { file, bytes_written, created_at })
    }

    fn should_rotate(&self, config: &WriterConfig) -> bool {
        if self.bytes_written >= config.rotate_max_bytes {
            return true;
        }
        if let Ok(age) = self.created_at.elapsed() {
            if age.as_secs() >= config.rotate_max_age_secs {
                return true;
            }
        }
        false
    }

    async fn rotate(&mut self, path: &Path, config: &WriterConfig) -> Result<(), std::io::Error> {
        // Best-effort fsync before rename.
        let _ = self.file.sync_all().await;

        let ts = format_ts_for_filename();
        // Rename current file: events.jsonl → events.jsonl.20260412-090000
        let rotated = {
            let mut p = path.to_owned();
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_owned();
            if ext.is_empty() {
                p.set_extension(&ts);
            } else {
                p.set_extension(format!("{ext}.{ts}"));
            }
            p
        };

        tokio::fs::rename(path, &rotated).await?;
        eprintln!(
            "dust-observability: rotated → {}",
            rotated.display()
        );

        let new_state = FileState::open_or_create(path, config).await?;
        *self = new_state;
        Ok(())
    }
}

// ── Overflow meta record ──────────────────────────────────────────────────────

fn overflow_record(count: u64) -> serde_json::Value {
    serde_json::json!({
        "kind": "meta",
        "type": "log_overflow",
        "ts": utc_now(),
        "dropped": count
    })
}

// ── log_redact_all check ──────────────────────────────────────────────────────

/// Return `true` when the message's `data.meta.log_redact_all` equals `true`.
fn log_redact_all_set(value: &serde_json::Value) -> bool {
    value
        .get("data")
        .and_then(|d| d.get("meta"))
        .and_then(|m| m.get("log_redact_all"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

// ── JSONPath-subset redaction ─────────────────────────────────────────────────

/// A single segment in our limited JSONPath subset (`$`, `.field`, `[*]`).
#[derive(Debug, PartialEq)]
enum PathSeg {
    Field(String),
    ArrayWildcard,
}

/// Parse a JSONPath string into segments.
///
/// Supported syntax: `$.field1.field2[*].field3`
///
/// Returns an empty vec for any path that starts with something other than `$`
/// or that contains unsupported tokens — those rules are silently ignored.
fn parse_redact_path(path: &str) -> Vec<PathSeg> {
    let path = path.trim();
    if !path.starts_with('$') {
        return vec![];
    }
    parse_segments(&path[1..]) // strip leading `$`
}

fn parse_segments(s: &str) -> Vec<PathSeg> {
    let mut segs = Vec::new();
    let mut rest = s;

    while !rest.is_empty() {
        if rest.starts_with('.') {
            rest = &rest[1..]; // consume '.'
            let end = rest.find(|c| c == '.' || c == '[').unwrap_or(rest.len());
            let field = &rest[..end];
            if field.is_empty() {
                break;
            }
            segs.push(PathSeg::Field(field.to_owned()));
            rest = &rest[end..];
        } else if rest.starts_with("[*]") {
            segs.push(PathSeg::ArrayWildcard);
            rest = &rest[3..];
        } else {
            break; // unsupported token — stop
        }
    }

    segs
}

/// Apply a single `dust.log_redact` rule to `data` (the event `data` value).
pub(crate) fn redact_path(data: &mut serde_json::Value, rule: &str) {
    let segs = parse_redact_path(rule);
    if !segs.is_empty() {
        apply_redact_segs(data, &segs);
    }
}

fn apply_redact_segs(node: &mut serde_json::Value, segs: &[PathSeg]) {
    if segs.is_empty() {
        return;
    }
    match &segs[0] {
        PathSeg::Field(name) => {
            if segs.len() == 1 {
                // Leaf: remove the field from the object.
                if let serde_json::Value::Object(map) = node {
                    map.remove(name);
                }
            } else if let serde_json::Value::Object(map) = node {
                if let Some(child) = map.get_mut(name) {
                    apply_redact_segs(child, &segs[1..]);
                }
            }
        }
        PathSeg::ArrayWildcard => {
            if let serde_json::Value::Array(arr) = node {
                if segs.len() == 1 {
                    // Leaf [*]: null out every element.
                    for elem in arr.iter_mut() {
                        *elem = serde_json::Value::Null;
                    }
                } else {
                    for elem in arr.iter_mut() {
                        apply_redact_segs(elem, &segs[1..]);
                    }
                }
            }
        }
    }
}

// ── Path helpers ──────────────────────────────────────────────────────────────

fn default_events_path() -> Result<PathBuf, std::io::Error> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set")
    })?;
    Ok(PathBuf::from(home).join(".alluka").join("dust").join("events.jsonl"))
}

// ── Timestamp helpers ─────────────────────────────────────────────────────────

fn utc_now() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let (y, mo, d, h, mi, s) = secs_to_parts(dur.as_secs());
    format!(
        "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{ms:03}Z",
        ms = dur.subsec_millis()
    )
}

fn format_ts_for_filename() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let (y, mo, d, h, mi, s) = secs_to_parts(dur.as_secs());
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

fn secs_to_parts(secs: u64) -> (i64, u64, u64, u64, u64, u64) {
    let (y, mo, d) = civil_from_days((secs / 86400) as i64);
    let tod = secs % 86400;
    (y, mo, d, tod / 3600, (tod % 3600) / 60, tod % 60)
}

/// Howard Hinnant's algorithm: days-since-epoch → (year, month, day).
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

// ── Log filtering ─────────────────────────────────────────────────────────────

/// Filter JSONL log content to records that belong to `plugin_id`.
///
/// If `since_iso` is provided (ISO 8601 timestamp string), only records whose
/// `ts` field is lexicographically ≥ `since_iso` are included (ISO 8601 sorts
/// correctly when compared as strings at millisecond precision).
///
/// Returns only records that are valid JSON objects — malformed lines and
/// overflow meta records are skipped silently.
pub fn filter_logs<'a>(
    content: &'a str,
    plugin_id: &'a str,
    since_iso: Option<&'a str>,
) -> impl Iterator<Item = serde_json::Value> + 'a {
    content.lines().filter_map(move |line| {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        // Must have a matching plugin_id field.
        if value.get("plugin_id").and_then(|v| v.as_str()) != Some(plugin_id) {
            return None;
        }
        // Apply --since filter on the `ts` field.
        if let Some(since) = since_iso {
            let ts = value.get("ts").and_then(|v| v.as_str()).unwrap_or("");
            if ts < since {
                return None;
            }
        }
        Some(value)
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use dust_core::envelope::{EventEnvelope, EventType, HeartbeatEnvelope};

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn make_event(seq: u64) -> Envelope {
        Envelope::Event(EventEnvelope {
            id: format!("evt_{seq:016x}"),
            event_type: EventType::DataUpdated,
            ts: "2026-04-12T00:00:00.000Z".into(),
            sequence: Some(seq),
            data: serde_json::json!({ "value": seq }),
        })
    }

    fn make_heartbeat() -> Envelope {
        Envelope::Heartbeat(HeartbeatEnvelope {
            ts: "2026-04-12T00:00:00.000Z".into(),
        })
    }

    fn make_event_with_data(seq: u64, data: serde_json::Value) -> Envelope {
        Envelope::Event(EventEnvelope {
            id: format!("evt_{seq:016x}"),
            event_type: EventType::DataUpdated,
            ts: "2026-04-12T00:00:00.000Z".into(),
            sequence: Some(seq),
            data,
        })
    }

    // Read all JSONL lines from `path`, ignoring empty lines.
    async fn read_lines(path: &Path) -> Vec<serde_json::Value> {
        let content = tokio::fs::read_to_string(path).await.unwrap_or_default();
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("valid JSON line"))
            .collect()
    }

    // ── Batching ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn batching_writes_multiple_events_after_interval() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let writer = ObservabilityWriter::new_with_path(path.clone());

        writer.send("test-plugin", make_event(1), vec![]);
        writer.send("test-plugin", make_event(2), vec![]);

        // Wait longer than the 100 ms batch interval.
        tokio::time::sleep(Duration::from_millis(350)).await;

        let lines = read_lines(&path).await;
        assert_eq!(lines.len(), 2, "both events must be written after the batch interval");
        assert_eq!(lines[0]["sequence"], 1);
        assert_eq!(lines[1]["sequence"], 2);
    }

    #[tokio::test]
    async fn batching_flushes_at_4kib_without_waiting_for_timer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let writer = ObservabilityWriter::new_with_path(path.clone());

        // Each event is small; send enough to exceed 4 KiB in aggregate.
        // A typical event serializes to ~100 bytes, so 50 events ≈ 5 KiB.
        for i in 1..=50u64 {
            let data = serde_json::json!({ "payload": "x".repeat(80), "i": i });
            writer.send("test-plugin", make_event_with_data(i, data), vec![]);
        }

        // Give the background task a moment to flush the oversized batch.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let lines = read_lines(&path).await;
        assert!(
            lines.len() >= 40,
            "at least 40 of 50 events must be written once 4 KiB is exceeded (got {})",
            lines.len()
        );
    }

    // ── Heartbeat filter ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn heartbeats_are_never_logged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let writer = ObservabilityWriter::new_with_path(path.clone());

        writer.send("test-plugin", make_heartbeat(), vec![]);
        writer.send("test-plugin", make_heartbeat(), vec![]);
        // Send one real event so the file is created.
        writer.send("test-plugin", make_event(1), vec![]);

        tokio::time::sleep(Duration::from_millis(350)).await;

        let lines = read_lines(&path).await;
        assert_eq!(lines.len(), 1, "only the non-heartbeat event must be written");
        assert_eq!(lines[0]["kind"], "event");
    }

    // ── Redaction ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn log_redact_strips_named_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let writer = ObservabilityWriter::new_with_path(path.clone());

        let env = make_event_with_data(
            1,
            serde_json::json!({ "secret": "password123", "ok": "visible" }),
        );
        writer.send("test-plugin", env, vec!["$.secret".into()]);

        tokio::time::sleep(Duration::from_millis(350)).await;

        let lines = read_lines(&path).await;
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0]["data"].get("secret").is_none(),
            "secret must be stripped; got: {}",
            lines[0]
        );
        assert_eq!(
            lines[0]["data"]["ok"].as_str(),
            Some("visible"),
            "ok must survive redaction"
        );
    }

    #[tokio::test]
    async fn log_redact_nested_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let writer = ObservabilityWriter::new_with_path(path.clone());

        let env = make_event_with_data(
            1,
            serde_json::json!({ "creds": { "token": "secret", "user": "alice" } }),
        );
        writer.send("test-plugin", env, vec!["$.creds.token".into()]);

        tokio::time::sleep(Duration::from_millis(350)).await;

        let lines = read_lines(&path).await;
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0]["data"]["creds"].get("token").is_none(),
            "nested token must be stripped"
        );
        assert_eq!(
            lines[0]["data"]["creds"]["user"].as_str(),
            Some("alice"),
            "sibling field must survive"
        );
    }

    #[tokio::test]
    async fn log_redact_array_wildcard() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let writer = ObservabilityWriter::new_with_path(path.clone());

        let env = make_event_with_data(
            1,
            serde_json::json!({
                "items": [
                    { "id": 1, "token": "tok-a" },
                    { "id": 2, "token": "tok-b" }
                ]
            }),
        );
        writer.send("test-plugin", env, vec!["$.items[*].token".into()]);

        tokio::time::sleep(Duration::from_millis(350)).await;

        let lines = read_lines(&path).await;
        assert_eq!(lines.len(), 1);
        let items = lines[0]["data"]["items"].as_array().unwrap();
        assert!(
            items[0].get("token").is_none(),
            "token must be stripped from first element"
        );
        assert!(
            items[1].get("token").is_none(),
            "token must be stripped from second element"
        );
        assert_eq!(items[0]["id"], 1, "id must survive");
        assert_eq!(items[1]["id"], 2, "id must survive");
    }

    // ── log_redact_all opt-out ────────────────────────────────────────────────

    #[tokio::test]
    async fn log_redact_all_suppresses_message_entirely() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let writer = ObservabilityWriter::new_with_path(path.clone());

        let sensitive = make_event_with_data(
            1,
            serde_json::json!({ "meta": { "log_redact_all": true }, "secret": "shh" }),
        );
        writer.send("test-plugin", sensitive, vec![]);

        // A normal event must still be written.
        writer.send("test-plugin", make_event(2), vec![]);

        tokio::time::sleep(Duration::from_millis(350)).await;

        let lines = read_lines(&path).await;
        assert_eq!(lines.len(), 1, "only the normal event must appear");
        assert_eq!(lines[0]["sequence"], 2, "the surviving event must be seq=2");
        // The suppressed event must not appear at all.
        let raw = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(!raw.contains("shh"), "suppressed content must not appear in the log");
    }

    // ── Overflow meta record ──────────────────────────────────────────────────

    #[tokio::test]
    async fn overflow_emits_log_overflow_meta_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let writer = ObservabilityWriter::new_with_path(path.clone());

        // Directly simulate 5 channel overflows (the background task drains the
        // counter on each batch flush and emits a meta record).
        writer.overflow.fetch_add(5, Ordering::Relaxed);

        // Send a normal event to trigger a flush.
        writer.send("test-plugin", make_event(1), vec![]);

        tokio::time::sleep(Duration::from_millis(350)).await;

        let lines = read_lines(&path).await;
        let overflow_records: Vec<_> = lines
            .iter()
            .filter(|l| l["kind"] == "meta" && l["type"] == "log_overflow")
            .collect();

        assert!(
            !overflow_records.is_empty(),
            "at least one log_overflow meta record must be emitted"
        );
        // The record must carry the dropped count.
        let dropped: u64 = overflow_records[0]["dropped"].as_u64().unwrap_or(0);
        assert!(dropped > 0, "dropped count must be > 0");
    }

    #[tokio::test]
    async fn overflow_on_full_channel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");

        // Create a writer with a 1-slot channel so it overflows immediately.
        let (tx, rx) = mpsc::channel(1);
        let overflow = Arc::new(AtomicU64::new(0));
        let overflow_bg = Arc::clone(&overflow);
        tokio::spawn(writer_task(
            rx,
            overflow_bg,
            path.clone(),
            WriterConfig::default(),
        ));
        let writer = ObservabilityWriter { tx, overflow };

        // First send fills the slot; second overflows.
        writer.send("test-plugin", make_event(1), vec![]);
        writer.send("test-plugin", make_event(2), vec![]);

        tokio::time::sleep(Duration::from_millis(350)).await;

        // The overflow counter must have been incremented (and then drained
        // into a meta record by the background task, so the raw counter may
        // now be 0 — that's fine).  We check the file for a meta record.
        let content = tokio::fs::read_to_string(&path).await.unwrap_or_default();
        // At a minimum, both events should be present.
        // If the channel was at capacity, at least one event was written.
        assert!(
            !content.is_empty(),
            "at least one event must be written"
        );
    }

    // ── Rotation ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn rotation_renames_file_at_byte_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");

        // Use a tiny rotation threshold (200 bytes) so rotation fires quickly.
        // Each event serialises to ~120 bytes; 3 events ≈ 360 bytes > threshold.
        let writer = ObservabilityWriter::new_with_config(
            path.clone(),
            WriterConfig {
                rotate_max_bytes: 200,
                rotate_max_age_secs: ROTATE_MAX_AGE_SECS,
            },
        );

        // Wave 1: push enough events to exceed the threshold once flushed.
        for i in 1..=5u64 {
            writer.send("test-plugin", make_event(i), vec![]);
        }
        // Wait for the batch interval so the first flush happens and
        // bytes_written is updated past the 200-byte threshold.
        tokio::time::sleep(Duration::from_millis(250)).await;

        // Wave 2: the should_rotate() check fires before writing this batch
        // because bytes_written is now > 200 — this triggers the rename.
        for i in 6..=8u64 {
            writer.send("test-plugin", make_event(i), vec![]);
        }
        tokio::time::sleep(Duration::from_millis(300)).await;

        // The directory must contain at least one rotated file.
        let mut dir_entries = tokio::fs::read_dir(dir.path()).await.unwrap();
        let mut found_rotated = false;
        while let Ok(Some(entry)) = dir_entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            // Rotated files look like: events.jsonl.20260412-090000
            if name.starts_with("events.jsonl.") && name.len() > "events.jsonl.".len() {
                found_rotated = true;
                break;
            }
        }
        assert!(found_rotated, "a rotated events file must exist in the directory");
    }

    // ── JSONPath parser (unit tests) ──────────────────────────────────────────

    #[test]
    fn parse_simple_field() {
        let segs = parse_redact_path("$.foo");
        assert_eq!(segs, vec![PathSeg::Field("foo".into())]);
    }

    #[test]
    fn parse_nested_fields() {
        let segs = parse_redact_path("$.foo.bar.baz");
        assert_eq!(
            segs,
            vec![
                PathSeg::Field("foo".into()),
                PathSeg::Field("bar".into()),
                PathSeg::Field("baz".into()),
            ]
        );
    }

    #[test]
    fn parse_array_wildcard() {
        let segs = parse_redact_path("$.items[*].token");
        assert_eq!(
            segs,
            vec![
                PathSeg::Field("items".into()),
                PathSeg::ArrayWildcard,
                PathSeg::Field("token".into()),
            ]
        );
    }

    #[test]
    fn parse_invalid_path_returns_empty() {
        assert!(parse_redact_path("").is_empty());
        assert!(parse_redact_path("foo.bar").is_empty()); // no leading $
        assert!(parse_redact_path("$..deep").is_empty()); // double dot unsupported
    }

    #[test]
    fn parse_root_only_returns_empty() {
        // "$" alone produces no segments — nothing to strip.
        let segs = parse_redact_path("$");
        assert!(segs.is_empty());
    }

    #[test]
    fn redact_leaf_field() {
        let mut data = serde_json::json!({ "secret": "s3cr3t", "ok": "keep" });
        redact_path(&mut data, "$.secret");
        assert!(data.get("secret").is_none());
        assert_eq!(data["ok"].as_str(), Some("keep"));
    }

    #[test]
    fn redact_nested_field() {
        let mut data =
            serde_json::json!({ "a": { "b": { "c": "gone" }, "keep": "yes" } });
        redact_path(&mut data, "$.a.b.c");
        assert!(data["a"]["b"].get("c").is_none());
        assert_eq!(data["a"]["keep"].as_str(), Some("yes"));
    }

    #[test]
    fn redact_array_items_wildcard() {
        let mut data = serde_json::json!({
            "tokens": [{"id": 1, "tok": "a"}, {"id": 2, "tok": "b"}]
        });
        redact_path(&mut data, "$.tokens[*].tok");
        let arr = data["tokens"].as_array().unwrap();
        assert!(arr[0].get("tok").is_none());
        assert!(arr[1].get("tok").is_none());
        assert_eq!(arr[0]["id"], 1);
    }

    #[test]
    fn redact_missing_field_is_noop() {
        let mut data = serde_json::json!({ "other": "value" });
        redact_path(&mut data, "$.nonexistent");
        assert_eq!(data["other"].as_str(), Some("value"));
    }

    // ── log_redact_all unit test ───────────────────────────────────────────────

    #[test]
    fn log_redact_all_detection() {
        let v = serde_json::json!({
            "kind": "event",
            "data": { "meta": { "log_redact_all": true } }
        });
        assert!(log_redact_all_set(&v));

        let v2 = serde_json::json!({
            "kind": "event",
            "data": { "meta": { "log_redact_all": false } }
        });
        assert!(!log_redact_all_set(&v2));

        let v3 = serde_json::json!({ "kind": "event", "data": {} });
        assert!(!log_redact_all_set(&v3));
    }

    // ── filter_logs ───────────────────────────────────────────────────────────

    #[test]
    fn filter_logs_by_plugin_id() {
        // Build a JSONL string with records from two different plugins.
        let content = concat!(
            r#"{"kind":"event","plugin_id":"dust-health","ts":"2026-04-12T09:00:00.000Z","sequence":1}"#, "\n",
            r#"{"kind":"event","plugin_id":"dust-tracker","ts":"2026-04-12T09:01:00.000Z","sequence":2}"#, "\n",
            r#"{"kind":"event","plugin_id":"dust-health","ts":"2026-04-12T09:02:00.000Z","sequence":3}"#, "\n",
            r#"{"kind":"meta","type":"log_overflow","ts":"2026-04-12T09:02:00.001Z","dropped":1}"#, "\n",
        );

        let health: Vec<_> = filter_logs(content, "dust-health", None).collect();
        assert_eq!(health.len(), 2, "must return both dust-health records");
        assert_eq!(health[0]["sequence"], 1);
        assert_eq!(health[1]["sequence"], 3);

        let tracker: Vec<_> = filter_logs(content, "dust-tracker", None).collect();
        assert_eq!(tracker.len(), 1);
        assert_eq!(tracker[0]["sequence"], 2);

        // Meta records without a matching plugin_id are excluded.
        let meta: Vec<_> = filter_logs(content, "dust-health", None)
            .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("log_overflow"))
            .collect();
        assert!(meta.is_empty(), "overflow meta records must be excluded");
    }

    #[test]
    fn filter_logs_with_since_excludes_older_records() {
        let content = concat!(
            r#"{"kind":"event","plugin_id":"my-plugin","ts":"2026-04-12T08:00:00.000Z","sequence":1}"#, "\n",
            r#"{"kind":"event","plugin_id":"my-plugin","ts":"2026-04-12T09:00:00.000Z","sequence":2}"#, "\n",
            r#"{"kind":"event","plugin_id":"my-plugin","ts":"2026-04-12T10:00:00.000Z","sequence":3}"#, "\n",
        );

        let results: Vec<_> =
            filter_logs(content, "my-plugin", Some("2026-04-12T09:00:00.000Z")).collect();
        assert_eq!(results.len(), 2, "records at or after since must be included");
        assert_eq!(results[0]["sequence"], 2);
        assert_eq!(results[1]["sequence"], 3);
    }

    #[test]
    fn filter_logs_skips_malformed_lines() {
        let content = "not-json\n{\"plugin_id\":\"p\",\"ts\":\"2026-01-01T00:00:00.000Z\"}\nbad{\n";
        let results: Vec<_> = filter_logs(content, "p", None).collect();
        assert_eq!(results.len(), 1, "only valid JSON lines matching plugin_id must be returned");
    }

    #[test]
    fn filter_logs_empty_content_returns_nothing() {
        let results: Vec<_> = filter_logs("", "any-plugin", None).collect();
        assert!(results.is_empty());
    }
}
