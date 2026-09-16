//! Read-only, bounded replay and follow support for pilot progress artifacts.
//!
//! This module deliberately accepts a wider display vocabulary than `codex`.
//! Rendering an observation never changes the executor's fail-closed parser.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) mod framing;
mod providers;
use framing::{Framer, Line};
pub(crate) use providers::safe_text;
use providers::{ClaudeBlock, is_process_output, normalize, provider_observations, redact};

use serde_json::{Value, json};
use thiserror::Error;

use crate::PilotOptions;

const SCHEMA: &str = "nanika.rust-observation-feed.v1";
const READ_BYTES: usize = 8 * 1024;
const MAX_FRAME_BYTES: usize = 256 * 1024;
const MAX_SNIPPET_BYTES: usize = 8 * 1024;
const FOLLOW_WAIT: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OutputFormat {
    Text,
    Json,
}

impl OutputFormat {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "text" => Some(Self::Text),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum ObserveError {
    #[error("progress log {path} is not a regular file")]
    NotRegular { path: PathBuf },
    #[error("opening progress log {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("reading progress log {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("progress log {path} was replaced or truncated while following")]
    SourceChanged { path: PathBuf },
    #[error("writing observation output: {0}")]
    Output(#[from] io::Error),
}

#[derive(Clone, Copy)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    fn inspect(path: &Path) -> Result<(Self, u64), ObserveError> {
        let metadata = fs::symlink_metadata(path).map_err(|source| ObserveError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(ObserveError::NotRegular {
                path: path.to_path_buf(),
            });
        }
        Ok((
            Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            metadata.len(),
        ))
    }

    fn still_matches(self, path: &Path, cursor: u64) -> Result<bool, ObserveError> {
        let (current, length) = Self::inspect(path)?;
        Ok(current.device == self.device && current.inode == self.inode && length >= cursor)
    }
}

/// Replays a saved progress artifact, or follows append-only bytes from it.
/// It opens no daemon, creates no sidecar, and has no cancellation authority.
pub(crate) fn run(options: &PilotOptions, output: &mut impl Write) -> Result<(), ObserveError> {
    let path = options
        .progress_log
        .as_deref()
        .ok_or_else(|| ObserveError::NotRegular {
            path: PathBuf::new(),
        })?;
    let (identity, _) = FileIdentity::inspect(path)?;
    let file = open_checked(path, identity)?;

    let mut reader = Reader::new(path, identity, file, options.observe_format, output);
    loop {
        let read = reader.read_available()?;
        if read != 0 {
            continue;
        }
        if !options.observe_follow {
            reader.finish_incomplete()?;
            return Ok(());
        }
        if !identity.still_matches(path, reader.cursor)? {
            return Err(ObserveError::SourceChanged {
                path: path.to_path_buf(),
            });
        }
        thread::sleep(FOLLOW_WAIT);
    }
}

fn open_checked(path: &Path, identity: FileIdentity) -> Result<File, ObserveError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)
        .map_err(|source| ObserveError::Open {
            path: path.to_path_buf(),
            source,
        })?;
    let opened = file.metadata().map_err(|source| ObserveError::Open {
        path: path.to_path_buf(),
        source,
    })?;
    if !opened.file_type().is_file()
        || opened.dev() != identity.device
        || opened.ino() != identity.inode
    {
        return Err(ObserveError::SourceChanged {
            path: path.to_path_buf(),
        });
    }

    Ok(file)
}

const MAX_STREAMS: usize = 32;
const MAX_DETAIL_BYTES: usize = 32 * 1024;
type StreamKey = (Option<String>, Option<String>);
type Stamp = (u64, i64, i64, i64, i64);

struct PrefixVerification {
    stamp: Stamp,
    offset: u64,
    hash: Sha256,
}

#[derive(Clone, Default)]
struct ProviderStream {
    framing: Framer,
    source_start: u64,
    session: Option<String>,
    turn: Option<String>,
    claude_blocks: BTreeMap<u64, ClaudeBlock>,
}

struct Reader<W> {
    path: PathBuf,
    identity: FileIdentity,
    file: File,
    format: OutputFormat,
    output: W,
    framing: Framer,
    cursor: u64,
    event_end: u64,
    sequence: u64,
    stamp: Option<Stamp>,
    verification: Option<PrefixVerification>,
    pending_read: Option<Vec<u8>>,
    #[cfg(test)]
    after_read: Option<Box<dyn FnOnce() -> io::Result<()>>>,
    prefix: Sha256,
    providers: BTreeMap<StreamKey, ProviderStream>,
}

impl<W: Write> Reader<W> {
    fn new(
        path: &Path,
        identity: FileIdentity,
        file: File,
        format: OutputFormat,
        output: W,
    ) -> Self {
        Self {
            path: path.to_path_buf(),
            identity,
            file,
            format,
            output,
            framing: Framer::default(),
            cursor: 0,
            event_end: 0,
            sequence: 0,
            stamp: None,
            verification: None,
            pending_read: None,
            #[cfg(test)]
            after_read: None,
            prefix: Sha256::new(),
            providers: BTreeMap::new(),
        }
    }

    fn changed(&self) -> ObserveError {
        ObserveError::SourceChanged {
            path: self.path.clone(),
        }
    }

    fn metadata(&self) -> Result<std::fs::Metadata, ObserveError> {
        if !self.identity.still_matches(&self.path, self.cursor)? {
            return Err(self.changed());
        }
        let metadata = self.file.metadata().map_err(|source| ObserveError::Read {
            path: self.path.clone(),
            source,
        })?;
        if metadata.dev() != self.identity.device
            || metadata.ino() != self.identity.inode
            || metadata.len() < self.cursor
        {
            return Err(self.changed());
        }
        Ok(metadata)
    }

    /// Verify the fixed consumed prefix, allowing append-only suffix growth.
    /// Both validation and pending reads stay bounded so the UI can process input.
    fn verify_source(&mut self) -> Result<bool, ObserveError> {
        let mut stamp = metadata_stamp(&self.metadata()?);
        if let Some(scan) = &mut self.verification {
            if scan.stamp != stamp {
                if stamp.0 > scan.stamp.0 {
                    // Suffix growth does not invalidate the fixed consumed
                    // prefix being scanned. Same-size rewrites still restart.
                    scan.stamp = stamp;
                } else {
                    self.verification = None;
                }
            }
        }
        if self.verification.is_none()
            && self.stamp.is_some_and(|previous| previous != stamp)
            && self.cursor != 0
        {
            self.verification = Some(PrefixVerification {
                stamp,
                offset: 0,
                hash: Sha256::new(),
            });
        }
        let path = self.path.clone();
        if let Some(scan) = &mut self.verification {
            let count = (self.cursor - scan.offset).min(READ_BYTES as u64) as usize;
            let mut bytes = [0u8; READ_BYTES];
            let count = self
                .file
                .read_at(&mut bytes[..count], scan.offset)
                .map_err(|source| ObserveError::Read {
                    path: path.clone(),
                    source,
                })?;
            if count == 0 {
                return Err(ObserveError::SourceChanged { path });
            }
            scan.hash.update(&bytes[..count]);
            scan.offset += count as u64;
            if scan.offset < self.cursor {
                return Ok(false);
            }
            let digest = scan.hash.clone().finalize();
            let after = metadata_stamp(&self.metadata()?);
            if after != stamp && after.0 <= stamp.0 {
                self.verification = None;
                return Ok(false);
            }
            stamp = after;
            if digest != self.prefix.clone().finalize() {
                return Err(self.changed());
            }
            self.verification = None;
        }
        self.stamp = Some(stamp);
        Ok(true)
    }

    fn read_available(&mut self) -> Result<usize, ObserveError> {
        if !self.verify_source()? {
            return Ok(usize::MAX);
        }
        if self.pending_read.is_none() {
            let mut chunk = vec![0u8; READ_BYTES];
            let count = self
                .file
                .read(&mut chunk)
                .map_err(|source| ObserveError::Read {
                    path: self.path.clone(),
                    source,
                })?;
            chunk.truncate(count);
            self.cursor = self.cursor.saturating_add(count as u64);
            self.prefix.update(&chunk);
            self.pending_read = Some(chunk);
            #[cfg(test)]
            if let Some(hook) = self.after_read.take() {
                hook()?;
            }
            // Keep newly read bytes private until post-read prefix validation
            // succeeds. A detected raced rewrite cannot publish a row first.
            if !self.verify_source()? {
                return Ok(usize::MAX);
            }
        }
        let chunk = self.pending_read.take().unwrap_or_default();
        let count = chunk.len();
        for byte in chunk {
            if let Some(line) = self.framing.push(byte) {
                self.frame(line)?;
            }
        }
        self.output.flush()?;
        Ok(count)
    }

    fn finish_incomplete(&mut self) -> Result<(), ObserveError> {
        self.event_end = self.cursor;
        if let Some(line) = self.framing.finish() {
            self.emit(
                if line.truncated {
                    "truncated"
                } else {
                    "incomplete"
                },
                json!({"reason":"unfinished outer frame at EOF", "bytes":line.end-line.start}),
                line.start,
            )?;
        }
        for (key, mut stream) in std::mem::take(&mut self.providers) {
            for block in stream.claude_blocks.values() {
                self.emit("incomplete", json!({"phase_id":key.0,"stream":key.1,"session_id":stream.session,"turn_id":stream.turn,"tool_use_id":block.id,"reason":"tool argument block remains open at EOF"}), stream.source_start)?;
            }
            if let Some(line) = stream.framing.finish() {
                self.emit(if line.truncated { "truncated" } else { "incomplete" },
                    json!({"reason":"unfinished provider line at EOF", "phase_id":key.0, "stream":key.1, "bytes":line.end-line.start}), stream.source_start)?;
            }
        }
        self.output.flush()?;
        Ok(())
    }

    fn frame(&mut self, line: Line) -> Result<(), ObserveError> {
        self.event_end = line.end;
        if line.truncated {
            for stream in self.providers.values_mut() {
                stream.framing.lose_sync();
                stream.session = None;
                stream.turn = None;
                stream.claude_blocks.clear();
            }
            return self.emit(
                "truncated",
                json!({"reason":"outer frame exceeds byte limit", "bytes":line.end-line.start}),
                line.start,
            );
        }
        if line.bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let value = match serde_json::from_slice::<Value>(&line.bytes) {
            Ok(value) => value,
            Err(_) => {
                if line.bytes.starts_with(b"pilot cancelled:") {
                    return self.emit("supervisor_cancelled", json!({"source":"pilot CLI diagnostic","reason":"invocation reported cancellation; free-form detail withheld","bytes":line.bytes.len()}), line.start);
                }
                if line.bytes.starts_with(b"pilot refused:")
                    || line.bytes.starts_with(b"pilot failed:")
                {
                    return self.emit("supervisor_error",json!({"source":"pilot CLI diagnostic","reason":"invocation reported failure; free-form detail withheld","bytes":line.bytes.len()}),line.start);
                }
                if let Ok(text) = std::str::from_utf8(&line.bytes) {
                    if let Some(status) = text
                        .strip_prefix("status=")
                        .and_then(|s| s.split_whitespace().next())
                    {
                        if matches!(status, "completed" | "failed" | "paused" | "cancelled") {
                            return self.emit(
                                "supervisor_status",
                                json!({"source":"pilot CLI summary","reported_status":status}),
                                line.start,
                            );
                        }
                    }
                }
                for stream in self.providers.values_mut() {
                    stream.framing.lose_sync();
                    stream.session = None;
                    stream.turn = None;
                    stream.claude_blocks.clear();
                }
                return self.emit("malformed",json!({"reason":"invalid JSON frame; raw preview withheld","bytes":line.bytes.len()}),line.start);
            }
        };
        if is_process_output(&value) {
            return self.provider_chunk(providers::redact_progress_chunk(value), line.start);
        }
        let value = redact(value);
        if matches!(
            value.get("kind").and_then(Value::as_str),
            Some("output_dropped" | "progress_dropped")
        ) {
            for stream in self.providers.values_mut() {
                stream.framing.lose_sync();
                stream.session = None;
                stream.turn = None;
                stream.claude_blocks.clear();
            }
        }
        if value.get("type").is_some() && value.get("schema").is_none() {
            let mut stream = self.providers.remove(&(None, None)).unwrap_or_default();
            self.provider_event(value, &mut stream, &None, &None, line.start)?;
            self.providers.insert((None, None), stream);
            return Ok(());
        }
        let (kind, detail) = normalize(&value);
        self.emit(&kind, json!({"detail":detail}), line.start)
    }

    fn provider_chunk(&mut self, value: Value, offset: u64) -> Result<(), ObserveError> {
        let phase = identifier(&value, "phase_id");
        let channel = identifier(&value, "stream");
        let text = value.get("text").and_then(Value::as_str);
        // Raw nested JSON must not bypass redaction in a wrapper or snippet.
        self.emit(
            "provider_chunk",
            json!({"phase_id":phase,"stream":channel,"bytes":text.map(str::len),
            "encoding":value.get("encoding"),"raw_preview":"withheld; see source artifact"}),
            offset,
        )?;
        let Some(text) = text else {
            return Ok(());
        };
        let key = (phase.clone(), channel.clone());
        if !self.providers.contains_key(&key) && self.providers.len() >= MAX_STREAMS {
            return self.emit("lost", json!({"phase_id":phase,"stream":channel,"reason":"provider stream limit", "bytes":text.len()}), offset);
        }
        let mut stream = self.providers.remove(&key).unwrap_or_default();
        for byte in text.bytes() {
            if !stream.framing.pending() {
                stream.source_start = offset;
            }
            let Some(line) = stream.framing.push(byte) else {
                continue;
            };
            if line.truncated {
                stream.session = None;
                stream.turn = None;
                stream.claude_blocks.clear();
                self.emit("truncated", json!({"phase_id":phase,"stream":channel,"reason":"provider line exceeds byte limit","bytes":line.end-line.start}), stream.source_start)?;
                continue;
            }
            if line.bytes.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let nested = match serde_json::from_slice::<Value>(&line.bytes) {
                Ok(value) => redact(value),
                Err(_) => {
                    self.emit("provider_line", json!({"phase_id":phase,"stream":channel,"bytes":line.bytes.len(),"raw_preview":"unstructured output withheld"}), stream.source_start)?;
                    continue;
                }
            };
            let source_start = stream.source_start;
            self.provider_event(nested, &mut stream, &phase, &channel, source_start)?;
        }
        self.providers.insert(key, stream);
        Ok(())
    }

    fn provider_event(
        &mut self,
        nested: Value,
        stream: &mut ProviderStream,
        phase: &Option<String>,
        channel: &Option<String>,
        offset: u64,
    ) -> Result<(), ObserveError> {
        match nested.get("type").and_then(Value::as_str) {
            Some("thread.started") => {
                stream.session = identifier(&nested, "thread_id");
                stream.turn = None;
                stream.claude_blocks.clear();
            }
            Some("turn.started") => {
                stream.turn = identifier(&nested, "turn_id");
            }
            _ => {}
        }
        if let Some(session) = identifier(&nested, "session_id") {
            stream.session = Some(session);
        }
        let provider_timestamp = nested
            .get("timestamp")
            .or_else(|| nested.get("created_at"))
            .cloned();
        for (kind, detail) in provider_observations(&nested, &mut stream.claude_blocks) {
            self.emit(
                &kind,
                json!({"phase_id":phase,"stream":channel,"session_id":stream.session,
                "turn_id":stream.turn,"provider_timestamp":provider_timestamp,"nested":detail}),
                offset,
            )?;
        }
        Ok(())
    }

    fn emit(&mut self, kind: &str, body: Value, offset: u64) -> Result<(), ObserveError> {
        self.sequence = self.sequence.saturating_add(1);
        let detail = body
            .get("nested")
            .or_else(|| body.get("detail"))
            .unwrap_or(&body);
        let item = detail.get("item").unwrap_or(detail);
        let provider = json!({
            "session_id":identifier(&body,"session_id").or_else(||identifier(detail,"thread_id")).or_else(||identifier(detail,"session_id")),
            "turn_id":identifier(&body,"turn_id").or_else(||identifier(detail,"turn_id")),
            "item_id":identifier(item,"id").or_else(||identifier(detail,"item_id")),
            "call_id":identifier(detail,"tool_use_id").or_else(||identifier(item,"tool_use_id")).or_else(||identifier(item,"call_id"))
                .or_else(||(detail["type"] == "tool_use").then(||identifier(detail,"id")).flatten()),
        });
        let mission_id =
            identifier(&body, "mission_id").or_else(|| identifier(detail, "mission_id"));
        let phase_id = identifier(&body, "phase_id").or_else(|| identifier(detail, "phase_id"));
        let provider_timestamp = body.get("provider_timestamp").cloned();
        let serialized = body.to_string();
        let complete = serialized.len() <= MAX_DETAIL_BYTES;
        let body = if complete {
            body
        } else {
            json!({"preview":safe_text(&serialized),"truncated":true,"original_bytes":serialized.len()})
        };
        let observation = json!({
            "schema":SCHEMA,"sequence":self.sequence,
            "source":{"artifact":self.path.display().to_string(),"device":self.identity.device,"inode":self.identity.inode,
                "byte_offset":offset,"cursor":self.event_end,"resume_requires_replay":true},
            "kind":kind,"observation_time_unix_ms":SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d|d.as_millis()),
            "provider_timestamp":provider_timestamp,"mission_id":mission_id,"phase_id":phase_id,
            "provider":provider,"complete":complete,"body":body,
        });
        match self.format {
            OutputFormat::Json => writeln!(self.output, "{observation}"),
            OutputFormat::Text => writeln!(
                self.output,
                "#{:06} {:<18} {}",
                self.sequence,
                kind,
                safe_text(&observation.to_string())
            ),
        }
        .map_err(ObserveError::from)
    }
}

#[derive(Default)]
struct ObservationBuffer {
    pending: Vec<u8>,
    rows: Vec<Value>,
}

impl Write for ObservationBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let json = line.strip_suffix(b"\n").unwrap_or(&line);
            let value = serde_json::from_slice(json)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            self.rows.push(value);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Bounded stepping access to the accepted O1 reader. Each step consumes at
/// most one physical read chunk and returns already-redacted observations.
pub(crate) struct ObservationFeed {
    reader: Reader<ObservationBuffer>,
    finished: bool,
}

pub(crate) struct FeedStep {
    pub(crate) observations: Vec<Value>,
    pub(crate) at_eof: bool,
}

impl ObservationFeed {
    pub(crate) fn open(path: &Path) -> Result<Self, ObserveError> {
        let (identity, _) = FileIdentity::inspect(path)?;
        let file = open_checked(path, identity)?;
        Ok(Self {
            reader: Reader::new(
                path,
                identity,
                file,
                OutputFormat::Json,
                ObservationBuffer::default(),
            ),
            finished: false,
        })
    }

    pub(crate) fn step(&mut self) -> Result<FeedStep, ObserveError> {
        let read = self.reader.read_available()?;
        if read != 0 && read != usize::MAX {
            self.finished = false;
        }
        Ok(FeedStep {
            observations: std::mem::take(&mut self.reader.output.rows),
            at_eof: read == 0,
        })
    }

    pub(crate) fn finish_replay(&mut self) -> Result<Vec<Value>, ObserveError> {
        if !self.finished {
            // Report the EOF snapshot without destroying state needed if the
            // operator subsequently follows this same append-only artifact.
            let framing = self.reader.framing.clone();
            let providers = self.reader.providers.clone();
            let result = self.reader.finish_incomplete();
            self.reader.framing = framing;
            self.reader.providers = providers;
            result?;
            self.finished = true;
        }
        Ok(std::mem::take(&mut self.reader.output.rows))
    }

    pub(crate) fn source_still_matches(&self) -> Result<bool, ObserveError> {
        self.reader
            .identity
            .still_matches(&self.reader.path, self.reader.cursor)
    }
}

fn metadata_stamp(metadata: &std::fs::Metadata) -> Stamp {
    (
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

fn identifier(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 1024)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::providers::bounded_unknown;
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn replay(bytes: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "nanika-observe-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, bytes)?;
        let (identity, _) = FileIdentity::inspect(&path)?;
        let mut output = Vec::new();
        {
            let file = File::open(&path)?;
            let mut reader = Reader::new(&path, identity, file, OutputFormat::Json, &mut output);
            while reader.read_available()? != 0 {}
            reader.finish_incomplete()?;
        }
        let _ = fs::remove_file(path);
        Ok(String::from_utf8(output)?)
    }

    #[test]
    fn intentional_cancellation_is_informational_and_withholds_raw_diagnostic()
    -> Result<(), Box<dyn std::error::Error>> {
        let output = replay(b"pilot cancelled: PRIVATE_DETAIL\npilot failed: PRIVATE_DETAIL\n")?;
        let events = output
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(events[0]["kind"], "supervisor_cancelled");
        assert_eq!(events[1]["kind"], "supervisor_error");
        assert!(!output.contains("PRIVATE_DETAIL"));
        Ok(())
    }

    #[test]
    fn post_read_rewrite_is_refused_before_any_observation_is_published()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!("observe-post-read-{}", std::process::id()));
        fs::write(&path, b"{\"type\":\"old\"}\n")?;
        let (identity, _) = FileIdentity::inspect(&path)?;
        let mut reader = Reader::new(
            &path,
            identity,
            File::open(&path)?,
            OutputFormat::Json,
            ObservationBuffer::default(),
        );
        let target = path.clone();
        reader.after_read = Some(Box::new(move || fs::write(target, b"{\"type\":\"new\"}\n")));
        assert!(reader.read_available().is_err());
        assert!(reader.output.rows.is_empty());
        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn continuous_suffix_appends_do_not_restart_a_fixed_prefix_scan()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!("observe-hot-append-{}", std::process::id()));
        let mut prefix = vec![b' '; READ_BYTES * 12];
        prefix[READ_BYTES * 12 - 1] = b'\n';
        fs::write(&path, prefix)?;
        let mut feed = ObservationFeed::open(&path)?;
        while !feed.step()?.at_eof {}
        let mut writer = OpenOptions::new().append(true).open(&path)?;
        let mut observed = false;
        for _ in 0..40 {
            writer.write_all(b"{\"type\":\"turn.started\"}\n")?;
            writer.flush()?;
            let step = feed.step()?;
            if step
                .observations
                .iter()
                .any(|row| row["kind"] == "provider_turn")
            {
                observed = true;
                break;
            }
        }
        assert!(
            observed,
            "following must show records while the writer continues appending"
        );
        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn incremental_validation_restarts_when_already_scanned_bytes_change()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!("observe-scan-race-{}", std::process::id()));
        let mut bytes = vec![b' '; READ_BYTES * 3];
        bytes[READ_BYTES * 3 - 1] = b'\n';
        fs::write(&path, &bytes)?;
        let mut feed = ObservationFeed::open(&path)?;
        while !feed.step()?.at_eof {}
        bytes.extend_from_slice(b"{\"type\":\"turn.started\"}\n");
        fs::write(&path, &bytes)?;
        assert!(feed.step()?.observations.is_empty()); // first prefix slice
        bytes[0] = b'x';
        fs::write(&path, &bytes)?;
        let mut refused = false;
        for _ in 0..16 {
            match feed.step() {
                Err(_) => {
                    refused = true;
                    break;
                }
                Ok(step) => assert!(step.observations.is_empty()),
            }
        }
        assert!(
            refused,
            "a changing prefix must not be accepted from mixed scan generations"
        );
        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn replay_eof_preserves_partial_provider_bytes_and_session_for_follow()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!("observe-eof-follow-{}", std::process::id()));
        let wrap = |text: &str| {
            format!(
                "{}\n",
                json!({"schema":"nanika.rust-pilot.progress.v1","kind":"process_output","phase_id":"a","stream":"stdout","text":text})
            )
        };
        fs::write(
            &path,
            wrap("{\"type\":\"thread.started\",\"thread_id\":\"session-a\"}\n{\"type\":\"item.sta"),
        )?;
        let mut feed = ObservationFeed::open(&path)?;
        while !feed.step()?.at_eof {}
        assert!(
            feed.finish_replay()?
                .iter()
                .any(|r| r["kind"] == "incomplete")
        );
        OpenOptions::new().append(true).open(&path)?.write_all(wrap("rted\",\"item\":{\"id\":\"call-a\",\"type\":\"command_execution\",\"command\":\"visible\"}}\n").as_bytes())?;
        let mut rows = Vec::new();
        loop {
            let step = feed.step()?;
            rows.extend(step.observations);
            if step.at_eof {
                break;
            }
        }
        let command = rows
            .iter()
            .find(|r| r["kind"] == "command_start")
            .ok_or("partial command was lost")?;
        assert_eq!(command["provider"]["session_id"], "session-a");
        assert_eq!(command["provider"]["item_id"], "call-a");
        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn claude_open_block_limit_and_eof_report_missing_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut data = String::new();
        for index in 0..17 {
            data.push_str(&format!("{}\n",json!({"type":"stream_event","event":{"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":format!("tool-{index}"),"name":"example","input":{}}}})));
        }
        let output = replay(data.as_bytes())?;
        let rows: Vec<Value> = output
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        assert_eq!(
            rows.iter()
                .filter(|row| row["kind"] == "tool_start")
                .count(),
            17
        );
        assert_eq!(rows.iter().filter(|row| row["kind"] == "lost").count(), 1);
        assert_eq!(
            rows.iter()
                .filter(|row| row["kind"] == "incomplete")
                .count(),
            16
        );
        assert!(
            rows.iter()
                .filter(|row| row["kind"] == "incomplete")
                .all(|row| row["provider"]["call_id"].as_str().is_some())
        );
        Ok(())
    }

    #[test]
    fn claude_user_text_retains_its_origin() -> Result<(), Box<dyn std::error::Error>> {
        let output=replay(format!("{}\n",json!({"type":"user","message":{"content":[{"type":"text","text":"hello"},{"type":"tool_result","tool_use_id":"call-1","content":"ok"}]}})).as_bytes())?;
        let rows: Vec<Value> = output
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        assert_eq!(rows[0]["kind"], "user_message");
        assert_eq!(rows[1]["kind"], "tool_result");
        assert_eq!(rows[1]["provider"]["call_id"], "call-1");
        Ok(())
    }

    #[test]
    fn malformed_transport_breaks_partial_provider_state() -> Result<(), Box<dyn std::error::Error>>
    {
        let wrapper = |text: &str| {
            format!(
                "{}\n",
                json!({"schema":"nanika.rust-pilot.progress.v1","kind":"process_output","phase_id":"a","stream":"stdout","text":text})
            )
        };
        let data = wrapper("{\"type\":\"turn.")
            + "{broken\n"
            + &wrapper("completed\"}\n{\"type\":\"turn.started\"}\n");
        let output = replay(data.as_bytes())?;
        let rows: Vec<Value> = output
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        assert!(!rows.iter().any(|row| row["kind"] == "usage"));
        assert!(rows.iter().any(|row| row["kind"] == "provider_turn"));
        Ok(())
    }

    #[test]
    fn nested_transport_shaped_objects_do_not_bypass_redaction() {
        let value = json!({"schema":"nanika.rust-pilot.progress.v1","kind":"process_output","text":" {\"password\":\"NESTED_SECRET\"} "});
        assert!(!redact(value).to_string().contains("NESTED_SECRET"));
    }

    #[test]
    fn normalize_keeps_codex_command_start_distinct_from_result() {
        assert_eq!(
            normalize(&json!({"type":"item.started","item":{"type":"command_execution","id":"c"}}))
                .0,
            "command_start"
        );
    }

    #[test]
    fn normalize_does_not_treat_claude_block_stop_as_tool_result() {
        assert_eq!(
            normalize(&json!({"type":"stream_event","event":{"type":"content_block_stop"}})).0,
            "tool_block_closed"
        );
    }

    #[test]
    fn redaction_removes_configured_values_before_unknown_snippets() {
        let rendered = bounded_unknown("future", &redact(json!({"password":"nope"}))).to_string();
        assert!(!rendered.contains("nope"));
    }

    #[test]
    fn safe_text_escapes_terminal_controls() {
        assert!(!safe_text("\u{1b}[2J").contains('\u{1b}'));
    }

    #[test]
    fn replay_keeps_interleaved_phase_chunks_as_separate_observations()
    -> Result<(), Box<dyn std::error::Error>> {
        let output = replay(b"{\"schema\":\"nanika.rust-pilot.progress.v1\",\"kind\":\"process_output\",\"phase_id\":\"a\"}\n{\"schema\":\"nanika.rust-pilot.progress.v1\",\"kind\":\"process_output\",\"phase_id\":\"b\"}\n")?;
        assert_eq!(output.lines().count(), 2);
        Ok(())
    }

    #[test]
    fn replay_marks_a_partial_final_json_frame_incomplete() -> Result<(), Box<dyn std::error::Error>>
    {
        assert!(replay(b"{\"type\":\"turn.started\"")?.contains("\"kind\":\"incomplete\""));
        Ok(())
    }

    #[test]
    fn replay_marks_malformed_and_oversize_frames_without_panicking()
    -> Result<(), Box<dyn std::error::Error>> {
        let oversized = vec![b'x'; MAX_FRAME_BYTES + 1];
        assert!(replay(&oversized)?.contains("truncated"));
        Ok(())
    }

    #[test]
    fn file_identity_refuses_a_directory_instead_of_treating_it_as_a_log()
    -> Result<(), Box<dyn std::error::Error>> {
        assert!(matches!(
            FileIdentity::inspect(Path::new(".")),
            Err(ObserveError::NotRegular { .. })
        ));
        Ok(())
    }

    #[test]
    fn observe_arguments_accept_only_the_read_only_surface()
    -> Result<(), Box<dyn std::error::Error>> {
        let options = crate::parse_arguments([
            "observe",
            "--progress-log",
            "saved.jsonl",
            "--format",
            "json",
        ])?;
        assert_eq!(options.observe_format, OutputFormat::Json);
        Ok(())
    }
    #[test]
    fn admitted_path_swapped_for_fifo_is_refused_without_blocking()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!("observe-fifo-swap-{}", std::process::id()));
        fs::write(&path, b"{}").map_err(ObserveError::from)?;
        let (identity, _) = FileIdentity::inspect(&path)?;
        fs::remove_file(&path)?;
        assert!(
            std::process::Command::new("/usr/bin/mkfifo")
                .arg(&path)
                .status()?
                .success()
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let target = path.clone();
        let thread = std::thread::spawn(move || {
            let _ = tx.send(open_checked(&target, identity).is_err());
        });
        let result = rx.recv_timeout(Duration::from_secs(2));
        fs::remove_file(path)?;
        assert!(result?, "swapped FIFO must be refused");
        thread.join().map_err(|_| "open-check thread panicked")?;
        Ok(())
    }

    #[test]
    fn claude_arguments_are_redacted_before_export_and_block_close_is_not_result()
    -> Result<(), Box<dyn std::error::Error>> {
        let events = [
            json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call-1","name":"query","input":{}}}}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"password\":\"SECRET"}}}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"VALUE\",\"query\":\"visible\"}"}}}),
            json!({"type":"stream_event","event":{"type":"content_block_stop","index":0}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"call-1","content":"result"}]}}),
        ];
        let bytes = events.iter().map(|e| format!("{e}\n")).collect::<String>();
        let output = replay(bytes.as_bytes())?;
        assert!(!output.contains("SECRET") && !output.contains("VALUE"));
        let rows: Vec<Value> = output
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        let input = rows
            .iter()
            .find(|r| r["kind"] == "tool_input")
            .ok_or("complete redacted arguments")?;
        assert_eq!(input["body"]["nested"]["arguments"]["query"], "visible");
        assert_eq!(
            input["body"]["nested"]["arguments"]["password"],
            "[REDACTED]"
        );
        assert_eq!(
            rows.iter().filter(|r| r["kind"] == "tool_result").count(),
            1
        );
        assert!(
            rows.iter()
                .filter(|r| matches!(
                    r["kind"].as_str(),
                    Some("tool_start" | "tool_input" | "tool_result" | "tool_block_closed")
                ))
                .all(|r| r["provider"]["call_id"] == "call-1")
        );
        Ok(())
    }

    #[test]
    fn interleaved_provider_frames_keep_sessions_and_exact_item_ids()
    -> Result<(), Box<dyn std::error::Error>> {
        let command = format!(
            "{}\n",
            json!({"type":"item.started","item":{"id":"item-a","type":"command_execution","command":"echo a"}})
        );
        let wrap = |phase: &str, text: String| json!({"schema":"nanika.rust-pilot.progress.v1","kind":"process_output","phase_id":phase,"stream":"stdout","text":text});
        let events = [
            wrap(
                "a",
                format!(
                    "{}\n{}",
                    json!({"type":"thread.started","thread_id":"session-a"}),
                    &command[..19]
                ),
            ),
            wrap(
                "b",
                format!(
                    "{}\n",
                    json!({"type":"thread.started","thread_id":"session-b"})
                ),
            ),
            wrap("a", command[19..].to_owned()),
        ];
        let output = replay(
            events
                .iter()
                .map(|e| format!("{e}\n"))
                .collect::<String>()
                .as_bytes(),
        )?;
        let rows: Vec<Value> = output
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        let tool = rows
            .iter()
            .find(|r| r["kind"] == "command_start")
            .ok_or("command start")?;
        assert_eq!(tool["provider"]["session_id"], "session-a");
        assert_eq!(tool["provider"]["item_id"], "item-a");
        assert_eq!(tool["phase_id"], "a");
        Ok(())
    }

    #[test]
    fn discarded_oversize_suffix_never_becomes_a_provider_event()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut bytes = vec![b'x'; MAX_FRAME_BYTES + READ_BYTES];
        bytes.extend_from_slice(b"{\"type\":\"turn.completed\"}\n{\"type\":\"turn.started\"}\n");
        let output = replay(&bytes)?;
        let rows: Vec<Value> = output
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        assert!(rows.iter().any(|r| r["kind"] == "truncated"));
        assert!(!rows.iter().any(|r| r["kind"] == "usage"));
        assert_eq!(
            rows.iter().filter(|r| r["kind"] == "provider_turn").count(),
            1
        );
        Ok(())
    }
}
