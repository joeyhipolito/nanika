//! File-backed **read** paths for the Go-compatible mission event log.
//!
//! Ported from the Go orchestrator's `internal/event` package —
//! `file.go`'s `LastSequence` and `project.go`'s `Projection`/
//! `ProjectFromLog` — plus the read-only core of `internal/cmd/events.go`'s
//! `list`/`replay`/`tail` subcommands (directory listing, incremental
//! tailing). Event JSON decoding is never re-implemented here: every reader
//! defers to [`orchestrator_core::decode_go_observed_event_record`]. This
//! module owns only capability-bounded directory/file access, Go-compatible
//! streaming line boundaries, and the mission/phase replay state machine.
//!
//! The live multi-writer emitter (`file.go`'s `FileEmitter`) and the bus/
//! `LiveState` subscription path are out of scope — this is a read-only
//! surface with no writer or actor.

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use cap_primitives::fs::{FollowSymlinks, open_dir_nofollow};
use cap_std::fs::{Dir, OpenOptions};
use thiserror::Error;

use orchestrator_core::{
    DecodedEvent, EventJsonMap, EventRecord, EventScanDiagnostic, EventScanDiagnosticKind,
    MissionId, STABLE_EVENT_TYPES, decode_go_observed_event_record,
};

const EVENTS_DIRECTORY: &str = "events";

/// Go's event readers configure `bufio.Scanner` with a 1 MiB maximum token.
/// The delimiter consumes one byte of that buffer, so the largest accepted
/// unterminated token is one byte smaller. The core constant records that
/// content ceiling; adding one recovers Scanner's configured buffer size.
const GO_EVENT_SCANNER_MAX_TOKEN_BYTES: usize =
    orchestrator_core::GO_EVENT_JSON_CONTENT_MAX_BYTES + 1;

/// Matches Go's default `bufio.Scanner` token ceiling used by
/// `internal/cmd/events.go`'s `countLines` (no explicit `scanner.Buffer`
/// call, so `bufio.MaxScanTokenSize` applies). The delimiter consumes one
/// byte of this buffer: at most 64 KiB - 1 bytes before LF are accepted.
const GO_DEFAULT_SCANNER_MAX_TOKEN_BYTES: usize = 64 * 1024;

/// Keeps one tail poll bounded when many complete records are waiting. A
/// single record may exceed this byte target because Go's tail reader has no
/// line ceiling and records cannot be split without changing observations.
const TAIL_BATCH_TARGET_BYTES: usize = 256 * 1024;
const TAIL_BATCH_MAX_RECORDS: usize = 256;

/// Failures from the read-only event-log surface.
#[derive(Debug, Error)]
pub enum EventLogIoError {
    /// A capability-bounded filesystem operation failed.
    #[error("event log operation failed during {operation}: {source}")]
    Io {
        /// Stable operation name that does not expose a caller path.
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// The events directory or a `.jsonl` entry has the wrong filesystem type.
    #[error("event log entry has the wrong type")]
    InvalidEntry,
    /// The requested mission has no event log to tail.
    #[error("no event log found for the requested mission")]
    NotFound,
    /// A line exceeded the scanner's byte ceiling. Matches Go's
    /// `bufio.Scanner` `ErrTooLong`: once a token overflows the scanner's
    /// buffer, `Scan()` returns `false` permanently and `scanner.Err()`
    /// becomes non-nil — the scan does not resume on the next line, and the
    /// caller (`LastSequence`, `ProjectFromLog`, `runEventsReplay`) treats
    /// the whole read as failed rather than returning partial data.
    #[error("event log line exceeds the scanner buffer and aborted the scan")]
    OversizedLine,
}

/// The minimal live snapshot of a mission derived from events (ported from
/// Go's `event.MissionSnap`, `livestate.go:11-17`). Structural plan data
/// (phases list, dependencies, objectives) is not carried — it lives in the
/// checkpoint.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MissionSnap {
    pub mission_id: String,
    /// One of `"in_progress"`, `"completed"`, `"failed"`, `"cancelled"`.
    pub status: String,
    /// RFC3339 timestamp, or empty when unset (Go zero-value-as-sentinel).
    pub started_at: String,
    /// RFC3339 timestamp, or empty when unset (Go zero-value-as-sentinel).
    pub ended_at: String,
    pub phases: BTreeMap<String, PhaseSnap>,
}

/// The minimal live snapshot of a phase derived from events (ported from
/// Go's `event.PhaseSnap`, `livestate.go:20-29`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PhaseSnap {
    pub id: String,
    pub name: String,
    /// One of `"running"`, `"completed"`, `"failed"`, `"skipped"`, or
    /// `"retrying"`. `"retrying"` is live-only: it never appears in a
    /// persisted checkpoint, only in a replayed event-log projection.
    pub status: String,
    pub started_at: String,
    pub ended_at: String,
}

/// A synchronous, single-pass replay of an event log into an in-memory
/// mission/phase snapshot. Ported from Go's `event.Projection`
/// (`project.go:15-97`) — unlike `LiveState` it has no bus subscription or
/// background goroutine, matching PORTING.md §2.8 bucket (a): a Go goroutine
/// used purely for a live subscription becomes plain sequential code once
/// there is no bus to subscribe to.
#[derive(Debug, Default)]
struct Projection {
    missions: BTreeMap<String, MissionSnap>,
}

impl Projection {
    fn mission(&self, mission_id: &str) -> Option<MissionSnap> {
        self.missions.get(mission_id).cloned()
    }

    fn apply(&mut self, event: &EventRecord) {
        let mission_id = event.mission_id.as_str();
        if mission_id.is_empty() {
            return;
        }
        match event.event_type.as_str() {
            "mission.started" => {
                if let Some(mission) = self.missions.get_mut(mission_id) {
                    mission.status = "in_progress".to_owned();
                } else {
                    self.missions.insert(
                        mission_id.to_owned(),
                        MissionSnap {
                            mission_id: mission_id.to_owned(),
                            status: "in_progress".to_owned(),
                            started_at: event.timestamp.clone(),
                            ended_at: String::new(),
                            phases: BTreeMap::new(),
                        },
                    );
                }
            }
            "mission.completed" => {
                self.set_mission_status(mission_id, "completed", &event.timestamp)
            }
            "mission.failed" => self.set_mission_status(mission_id, "failed", &event.timestamp),
            "mission.cancelled" => {
                self.set_mission_status(mission_id, "cancelled", &event.timestamp)
            }
            "phase.started" => {
                let Some(phase_id) = event.phase_id.as_deref() else {
                    return;
                };
                let mission = self.get_or_create_mission(mission_id, &event.timestamp);
                let name = data_string(event.data.as_ref(), "name");
                mission.phases.insert(
                    phase_id.to_owned(),
                    PhaseSnap {
                        id: phase_id.to_owned(),
                        name,
                        status: "running".to_owned(),
                        started_at: event.timestamp.clone(),
                        ended_at: String::new(),
                    },
                );
            }
            "phase.completed" => self.set_phase_status(mission_id, event, "completed"),
            "phase.failed" => self.set_phase_status(mission_id, event, "failed"),
            "phase.skipped" => self.set_phase_status(mission_id, event, "skipped"),
            "phase.retrying" => self.set_phase_status(mission_id, event, "retrying"),
            _ => {}
        }
    }

    fn get_or_create_mission(&mut self, mission_id: &str, timestamp: &str) -> &mut MissionSnap {
        self.missions
            .entry(mission_id.to_owned())
            .or_insert_with(|| MissionSnap {
                mission_id: mission_id.to_owned(),
                status: "in_progress".to_owned(),
                started_at: timestamp.to_owned(),
                ended_at: String::new(),
                phases: BTreeMap::new(),
            })
    }

    fn set_mission_status(&mut self, mission_id: &str, status: &str, timestamp: &str) {
        if let Some(mission) = self.missions.get_mut(mission_id) {
            mission.status = status.to_owned();
            mission.ended_at = timestamp.to_owned();
        }
    }

    fn set_phase_status(&mut self, mission_id: &str, event: &EventRecord, status: &str) {
        let Some(phase_id) = event.phase_id.as_deref() else {
            return;
        };
        let timestamp = event.timestamp.clone();
        let mission = self.get_or_create_mission(mission_id, &timestamp);
        if let Some(phase) = mission.phases.get_mut(phase_id) {
            phase.status = status.to_owned();
            phase.ended_at = timestamp;
        } else {
            let name = data_string(event.data.as_ref(), "name");
            mission.phases.insert(
                phase_id.to_owned(),
                PhaseSnap {
                    id: phase_id.to_owned(),
                    name,
                    status: status.to_owned(),
                    started_at: String::new(),
                    ended_at: timestamp,
                },
            );
        }
    }
}

fn data_string(data: Option<&EventJsonMap>, key: &str) -> String {
    data.and_then(|map| map.get(key))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

struct ScannedLine {
    raw: Vec<u8>,
    line_index: usize,
}

impl ScannedLine {
    fn content(&self) -> &[u8] {
        let token = self.raw.strip_suffix(b"\n").unwrap_or(&self.raw);
        token.strip_suffix(b"\r").unwrap_or(token)
    }
}

fn decoded_event(record: EventRecord, raw_line: Vec<u8>) -> DecodedEvent {
    let is_stable_type = STABLE_EVENT_TYPES.contains(&record.event_type.as_str());
    DecodedEvent {
        record,
        is_stable_type,
        raw_line,
    }
}

/// Record-at-a-time reader with Go `bufio.ScanLines` delimiter semantics.
/// `max_token_bytes == None` models Go tail's unbounded `ReadString` line.
struct GoLineReader<R> {
    reader: BufReader<R>,
    max_token_bytes: Option<usize>,
    raw_line: Vec<u8>,
    next_line_index: usize,
    finished: bool,
    deferred_error: Option<EventLogIoError>,
    operation: &'static str,
}

impl<R: Read> GoLineReader<R> {
    fn bounded(reader: R, max_token_bytes: usize, operation: &'static str) -> Self {
        debug_assert!(max_token_bytes > 0);
        Self {
            reader: BufReader::new(reader),
            max_token_bytes: Some(max_token_bytes),
            raw_line: Vec::new(),
            next_line_index: 1,
            finished: false,
            deferred_error: None,
            operation,
        }
    }

    fn unbounded(reader: R, operation: &'static str) -> Self {
        Self {
            reader: BufReader::new(reader),
            max_token_bytes: None,
            raw_line: Vec::new(),
            next_line_index: 1,
            finished: false,
            deferred_error: None,
            operation,
        }
    }

    fn next_line(&mut self) -> Result<Option<ScannedLine>, EventLogIoError> {
        if let Some(error) = self.deferred_error.take() {
            return Err(error);
        }
        if self.finished {
            return Ok(None);
        }

        loop {
            let available = match self.reader.fill_buf() {
                Ok(bytes) => bytes,
                Err(source) if source.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(source) => {
                    self.finished = true;
                    let error = EventLogIoError::Io {
                        operation: self.operation,
                        source,
                    };
                    if self.raw_line.is_empty() {
                        return Err(error);
                    }
                    self.deferred_error = Some(error);
                    return Ok(Some(self.take_line()));
                }
            };
            if available.is_empty() {
                self.finished = true;
                if self.raw_line.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(self.take_line()));
            }

            let newline = available.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(available.len(), |index| index + 1);
            let token_bytes = newline.unwrap_or(take);
            if self
                .max_token_bytes
                .is_some_and(|max| self.raw_line.len() + token_bytes >= max)
            {
                self.finished = true;
                return Err(EventLogIoError::OversizedLine);
            }
            self.raw_line.extend_from_slice(&available[..take]);
            self.reader.consume(take);
            if newline.is_some() {
                return Ok(Some(self.take_line()));
            }
        }
    }

    fn take_line(&mut self) -> ScannedLine {
        let line = ScannedLine {
            raw: std::mem::take(&mut self.raw_line),
            line_index: self.next_line_index,
        };
        self.next_line_index += 1;
        line
    }

    fn take_deferred_error(&mut self) -> Option<EventLogIoError> {
        self.deferred_error.take()
    }
}

/// Visits Go `bufio.ScanLines` tokens without buffering the whole reader.
/// `max_token_bytes` includes the LF delimiter, so exactly that many non-LF
/// bytes overflow while `max_token_bytes - 1` bytes followed by LF succeed.
fn scan_lines_like_go(
    reader: &mut impl Read,
    max_token_bytes: usize,
    mut visit: impl FnMut(&[u8], &[u8], bool, usize),
) -> Result<(), EventLogIoError> {
    debug_assert!(max_token_bytes > 0);

    // Projection, last-sequence, and list scans never transfer ownership of a
    // token. Keep one allocation and clear it after each callback instead of
    // using `GoLineReader::take_line`, whose owned-record contract must move
    // the allocation into every replay/tail result.
    let mut chunk = [0u8; 8 * 1024];
    let mut raw_line = Vec::with_capacity(chunk.len().min(max_token_bytes));
    let mut line_index = 1usize;

    loop {
        let read = match reader.read(&mut chunk) {
            Ok(0) => {
                if !raw_line.is_empty() {
                    let content = raw_line.strip_suffix(b"\r").unwrap_or(&raw_line);
                    visit(content, &raw_line, false, line_index);
                }
                return Ok(());
            }
            Ok(read) => read,
            Err(source) if source.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(source) => {
                if !raw_line.is_empty() {
                    let content = raw_line.strip_suffix(b"\r").unwrap_or(&raw_line);
                    visit(content, &raw_line, false, line_index);
                }
                return Err(EventLogIoError::Io {
                    operation: "read event log",
                    source,
                });
            }
        };

        for byte in &chunk[..read] {
            if *byte == b'\n' {
                let content = raw_line.strip_suffix(b"\r").unwrap_or(&raw_line);
                visit(content, &raw_line, true, line_index);
                raw_line.clear();
                line_index += 1;
                continue;
            }

            if raw_line.len() + 1 >= max_token_bytes {
                return Err(EventLogIoError::OversizedLine);
            }
            raw_line.push(*byte);
        }
    }
}

fn project_mission_in_reader(
    reader: &mut impl Read,
    mission_id: &str,
) -> Result<Option<MissionSnap>, EventLogIoError> {
    let mut projection = Projection::default();
    scan_lines_like_go(
        reader,
        GO_EVENT_SCANNER_MAX_TOKEN_BYTES,
        |content, _, _, _| {
            if let Ok(event) = decode_go_observed_event_record(content) {
                projection.apply(&event);
            }
        },
    )?;
    Ok(projection.mission(mission_id))
}

/// Replays `bytes` (one mission's JSONL history) and returns the resulting
/// mission snapshot, or `None` if no event for `mission_id` was seen.
/// Corrupt lines are skipped, matching Go's
/// `json.Unmarshal`-failure-then-`continue` in `ProjectFromLog`. An
/// oversized line aborts the whole scan and returns
/// [`EventLogIoError::OversizedLine`].
pub fn project_mission_in_bytes(
    bytes: &[u8],
    mission_id: &str,
) -> Result<Option<MissionSnap>, EventLogIoError> {
    let mut reader = bytes;
    project_mission_in_reader(&mut reader, mission_id)
}

fn last_sequence_in_reader(reader: &mut impl Read) -> Result<i64, EventLogIoError> {
    let mut greatest = 0i64;
    scan_lines_like_go(
        reader,
        GO_EVENT_SCANNER_MAX_TOKEN_BYTES,
        |content, _, _, _| {
            if let Ok(event) = decode_go_observed_event_record(content) {
                greatest = greatest.max(event.sequence);
            }
        },
    )?;
    Ok(greatest)
}

/// Returns the highest `sequence` found in `bytes`, or `0` for an empty log.
/// Corrupt lines are skipped, matching Go's `file.go:94-117` `LastSequence`.
/// An oversized line aborts the scan and returns
/// [`EventLogIoError::OversizedLine`], matching Go's `scanner.Err()` being
/// returned as a non-nil error rather than the partial max-so-far sequence.
pub fn last_sequence_in_bytes(bytes: &[u8]) -> Result<i64, EventLogIoError> {
    let mut reader = bytes;
    last_sequence_in_reader(&mut reader)
}

/// One mission log's directory-listing summary (ported from the core of
/// `internal/cmd/events.go`'s `runEventsList`, excluding table formatting).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissionLogSummary {
    pub mission_id: String,
    /// Non-empty raw line count, matching Go's `countLines` — a cheap
    /// approximation, not a validated event count (see that function's own
    /// doc comment).
    pub event_count: u64,
    pub size_bytes: u64,
}

/// One record in the exact order observed by a tail reader.
///
/// Both variants carry their half-open source byte range. A corrupt line is
/// data, not an iterator error: Go's `formatEventLine` prints its raw bytes.
#[derive(Clone, Debug, PartialEq)]
pub enum TailRecord {
    /// A Go-observable event envelope.
    Event {
        event: DecodedEvent,
        start_offset: u64,
        next_offset: u64,
    },
    /// A malformed envelope plus its exact bytes for raw fallback.
    Corrupt {
        diagnostic: EventScanDiagnostic,
        raw_line: Vec<u8>,
        start_offset: u64,
        next_offset: u64,
    },
}

impl TailRecord {
    /// Inclusive byte offset at which this record starts.
    #[must_use]
    pub fn start_offset(&self) -> u64 {
        match self {
            Self::Event { start_offset, .. } | Self::Corrupt { start_offset, .. } => *start_offset,
        }
    }

    /// Exclusive byte offset from which the next record can be read.
    #[must_use]
    pub fn next_offset(&self) -> u64 {
        match self {
            Self::Event { next_offset, .. } | Self::Corrupt { next_offset, .. } => *next_offset,
        }
    }
}

/// One bounded, ordered tail batch read since a prior offset.
#[derive(Debug, Default)]
pub struct TailRead {
    /// Events and corrupt raw lines in source order. [`tail_since`] and
    /// [`tail_reader`] return at most the configured record-count bound and
    /// stop at a record boundary after the byte target; [`tail_chunk`] is
    /// bounded by its caller-owned input slice instead.
    pub records: Vec<TailRecord>,
    /// The offset to pass to the next [`tail_since`] call. Go's
    /// `bufio.Reader.ReadString('\n')` returns non-empty bytes together with
    /// `io.EOF`; `runEventsTail` prints and consumes those bytes immediately,
    /// so this advances across unterminated EOF data too.
    pub next_offset: u64,
    /// A non-EOF read error encountered only after every record in `records`.
    /// The caller must consume the ordered records before surfacing this
    /// terminal error. When no record preceded the error, tail APIs return it
    /// directly instead of constructing a batch.
    pub deferred_error: Option<EventLogIoError>,
}

/// Consuming iterator for a [`TailRead`]. Any deferred read error is yielded
/// exactly once, after all ordered records in the batch.
pub struct TailRecords {
    records: std::vec::IntoIter<TailRecord>,
    deferred_error: Option<EventLogIoError>,
}

impl Iterator for TailRecords {
    type Item = Result<TailRecord, EventLogIoError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.records
            .next()
            .map(Ok)
            .or_else(|| self.deferred_error.take().map(Err))
    }
}

impl IntoIterator for TailRead {
    type Item = Result<TailRecord, EventLogIoError>;
    type IntoIter = TailRecords;

    fn into_iter(self) -> Self::IntoIter {
        TailRecords {
            records: self.records.into_iter(),
            deferred_error: self.deferred_error,
        }
    }
}

/// Opens the `events` subdirectory beneath `root`, no-follow. Returns `None`
/// when it does not exist yet — every caller in this module treats a missing
/// events directory the same way Go treats a missing log file.
fn open_events_directory(root: &Dir) -> Result<Option<Dir>, EventLogIoError> {
    match open_dir_nofollow_here(root, EVENTS_DIRECTORY) {
        Ok(directory) => {
            let metadata = directory
                .dir_metadata()
                .map_err(|source| EventLogIoError::Io {
                    operation: "inspect events directory",
                    source,
                })?;
            if !metadata.is_dir() {
                return Err(EventLogIoError::InvalidEntry);
            }
            Ok(Some(directory))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(EventLogIoError::Io {
            operation: "open events directory",
            source,
        }),
    }
}

fn open_log_file(
    events_directory: &Dir,
    mission_id: &MissionId,
) -> Result<Option<cap_std::fs::File>, EventLogIoError> {
    let file_name = format!("{}.jsonl", mission_id.as_str());
    let file = match open_file_nofollow_here(events_directory, &file_name) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(EventLogIoError::Io {
                operation: "open event log",
                source,
            });
        }
    };
    let metadata = file.metadata().map_err(|source| EventLogIoError::Io {
        operation: "inspect event log",
        source,
    })?;
    if !metadata.is_file() {
        return Err(EventLogIoError::InvalidEntry);
    }
    Ok(Some(file))
}

/// Returns the highest event `sequence` recorded for `mission_id`, or `0`
/// when the mission has no log yet — matching Go's `LastSequence` treating
/// a missing file as `(0, nil)`.
pub fn last_sequence(root: &Dir, mission_id: &MissionId) -> Result<i64, EventLogIoError> {
    let Some(events_directory) = open_events_directory(root)? else {
        return Ok(0);
    };
    let Some(mut file) = open_log_file(&events_directory, mission_id)? else {
        return Ok(0);
    };
    last_sequence_in_reader(&mut file)
}

/// Replays the persisted log for `mission_id` and returns its snapshot.
/// Returns `Ok(None)` (no error) when the log does not exist — matching
/// Go's `ProjectFromLog`, whose caller falls back to the checkpoint.
pub fn project_from_log(
    root: &Dir,
    mission_id: &MissionId,
) -> Result<Option<MissionSnap>, EventLogIoError> {
    let Some(events_directory) = open_events_directory(root)? else {
        return Ok(None);
    };
    let Some(mut file) = open_log_file(&events_directory, mission_id)? else {
        return Ok(None);
    };
    project_mission_in_reader(&mut file, mission_id.as_str())
}

/// Matches Go's `internal/cmd/events.go` `scanEvents`, the reader behind the
/// `replay` subcommand: an explicit `sc.Buffer(make([]byte, 64*1024),
/// 64*1024)` — a different, smaller ceiling than the 1 MiB
/// `GO_EVENT_SCANNER_MAX_TOKEN_BYTES` used for
/// `LastSequence`/`ProjectFromLog`. At most 64 KiB - 1 bytes before LF are
/// decoded; a token that fills the buffer aborts immediately (`sc.Err()` propagates out of
/// `runEventsReplay` as a command failure), matching
/// [`EventLogIoError::OversizedLine`] here.
const GO_REPLAY_SCANNER_MAX_TOKEN_BYTES: usize = 64 * 1024;

/// One record produced by [`ReplayRecords`]. Corrupt JSON is data rather than
/// an iterator error because Go's replay command prints that line verbatim.
#[derive(Clone, Debug, PartialEq)]
pub enum ReplayRecord {
    /// A Go-observable event envelope.
    Event(DecodedEvent),
    /// A malformed known envelope plus its exact bytes for raw fallback.
    Corrupt {
        diagnostic: EventScanDiagnostic,
        raw_line: Vec<u8>,
    },
}

/// Record-at-a-time replay iterator over a caller-supplied reader. Memory is
/// bounded to one scanner token and the record currently owned by the caller;
/// a scanner or I/O failure is yielded once and then terminates the iterator.
pub struct ReplayRecords<R: Read = cap_std::fs::File> {
    lines: GoLineReader<R>,
}

impl<R: Read> Iterator for ReplayRecords<R> {
    type Item = Result<ReplayRecord, EventLogIoError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let line = match self.lines.next_line() {
                Ok(Some(line)) => line,
                Ok(None) => return None,
                Err(error) => return Some(Err(error)),
            };
            if line.content().is_empty() {
                continue;
            }
            return Some(Ok(match decode_go_observed_event_record(line.content()) {
                Ok(record) => ReplayRecord::Event(decoded_event(record, line.raw)),
                Err(error) => ReplayRecord::Corrupt {
                    diagnostic: EventScanDiagnostic {
                        line_index: line.line_index,
                        kind: EventScanDiagnosticKind::Corrupt,
                        message: error.to_string(),
                    },
                    raw_line: line.raw,
                },
            }));
        }
    }
}

/// Builds a bounded record-at-a-time replay over an already-open reader.
/// This is the path-based CLI adapter point: a caller may validate and open a
/// direct `.jsonl` path itself, then transfer that file here without loading
/// the whole log or weakening the replay-specific 64 KiB token ceiling.
pub fn replay_reader<R: Read>(reader: R) -> ReplayRecords<R> {
    ReplayRecords {
        lines: GoLineReader::bounded(
            reader,
            GO_REPLAY_SCANNER_MAX_TOKEN_BYTES,
            "read event log for replay",
        ),
    }
}

/// Opens `mission_id` for record-at-a-time replay. This is the CLI-ready API
/// for large logs; callers should consume and render each item before asking
/// for the next one.
pub fn replay_records(
    root: &Dir,
    mission_id: &MissionId,
) -> Result<ReplayRecords, EventLogIoError> {
    let events_directory = open_events_directory(root)?.ok_or(EventLogIoError::NotFound)?;
    let file = open_log_file(&events_directory, mission_id)?.ok_or(EventLogIoError::NotFound)?;
    Ok(replay_reader(file))
}

/// Collects the persisted log for `mission_id` in file order. Returns
/// [`EventLogIoError::NotFound`] when the log is missing, matching Go's
/// `internal/cmd/events.go` `replay` subcommand, which errors via
/// `resolveLogPath` rather than treating a missing log as empty. Uses the
/// replay-specific 64 KiB scanner ceiling, not the
/// 1 MiB ceiling `LastSequence`/`ProjectFromLog` use — Go's `replay`
/// subcommand and its `LastSequence`/`ProjectFromLog` reads are two
/// different scanners with two different buffer sizes.
///
/// This compatibility wrapper intentionally retains all decoded records and
/// diagnostics. Streaming callers must use [`replay_records`].
pub fn replay(
    root: &Dir,
    mission_id: &MissionId,
) -> Result<(Vec<DecodedEvent>, Vec<EventScanDiagnostic>), EventLogIoError> {
    let mut events = Vec::new();
    let mut diagnostics = Vec::new();
    for record in replay_records(root, mission_id)? {
        match record? {
            ReplayRecord::Event(event) => events.push(event),
            ReplayRecord::Corrupt { diagnostic, .. } => diagnostics.push(diagnostic),
        }
    }
    Ok((events, diagnostics))
}

/// Reads newly appended records since `offset` and returns the next offset to
/// resume from. The batch stops at a record boundary after its byte target or
/// record-count ceiling; callers can continue immediately from `next_offset`.
/// One arbitrarily long record is still returned whole. Unterminated non-empty
/// EOF bytes are records too. Ported from the read side of
/// `internal/cmd/events.go`'s `runEventsTail`.
pub fn tail_since(
    root: &Dir,
    mission_id: &MissionId,
    offset: u64,
) -> Result<TailRead, EventLogIoError> {
    let events_directory = open_events_directory(root)?.ok_or(EventLogIoError::NotFound)?;
    let file_name = format!("{}.jsonl", mission_id.as_str());
    let file = match open_file_nofollow_here(&events_directory, &file_name) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(EventLogIoError::NotFound);
        }
        Err(source) => {
            return Err(EventLogIoError::Io {
                operation: "open event log for tail",
                source,
            });
        }
    };
    let metadata = file.metadata().map_err(|source| EventLogIoError::Io {
        operation: "inspect event log for tail",
        source,
    })?;
    if !metadata.is_file() {
        return Err(EventLogIoError::InvalidEntry);
    }
    tail_reader(file, offset)
}

/// Reads one bounded tail batch from an already-open seekable reader. The
/// reader is positioned at `offset` before scanning, so callers can safely
/// open a validated direct `.jsonl` file and reuse the same semantics as
/// [`tail_since`] without routing it through the capability-home layout.
/// The batch is bounded by record count and target bytes; as in Go, one
/// arbitrarily long record may exceed the byte target and is returned whole.
pub fn tail_reader<R: Read + Seek>(
    mut reader: R,
    offset: u64,
) -> Result<TailRead, EventLogIoError> {
    reader
        .seek(SeekFrom::Start(offset))
        .map_err(|source| EventLogIoError::Io {
            operation: "seek event log for tail",
            source,
        })?;
    read_tail_batch(reader, offset)
}

fn read_tail_batch<R: Read>(reader: R, base_offset: u64) -> Result<TailRead, EventLogIoError> {
    let mut lines = GoLineReader::unbounded(reader, "read event log for tail");
    let mut read = TailRead {
        next_offset: base_offset,
        ..TailRead::default()
    };
    let mut consumed = 0usize;
    while read.records.len() < TAIL_BATCH_MAX_RECORDS {
        let line = match lines.next_line() {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) if read.records.is_empty() => return Err(error),
            Err(error) => {
                read.deferred_error = Some(error);
                break;
            }
        };
        let start_offset = base_offset.saturating_add(consumed as u64);
        consumed = consumed.saturating_add(line.raw.len());
        let next_offset = base_offset.saturating_add(consumed as u64);
        push_tail_line(&mut read.records, line, start_offset, next_offset);
        if let Some(error) = lines.take_deferred_error() {
            read.deferred_error = Some(error);
            break;
        }
        if consumed >= TAIL_BATCH_TARGET_BYTES {
            break;
        }
    }
    read.next_offset = base_offset.saturating_add(consumed as u64);
    Ok(read)
}

fn push_tail_line(
    records: &mut Vec<TailRecord>,
    line: ScannedLine,
    start_offset: u64,
    next_offset: u64,
) {
    match decode_go_observed_event_record(line.content()) {
        Ok(record) => records.push(TailRecord::Event {
            event: decoded_event(record, line.raw),
            start_offset,
            next_offset,
        }),
        Err(error) => records.push(TailRecord::Corrupt {
            diagnostic: EventScanDiagnostic {
                line_index: line.line_index,
                kind: EventScanDiagnosticKind::Corrupt,
                message: error.to_string(),
            },
            raw_line: line.raw,
            start_offset,
            next_offset,
        }),
    }
}

/// Pure collecting chunk helper. Every non-empty EOF suffix is decoded or
/// reported immediately, matching Go's `ReadString` loop. Unlike
/// [`tail_since`], memory is caller-bounded only by `new_bytes`.
#[must_use]
pub fn tail_chunk(new_bytes: &[u8], base_offset: u64) -> TailRead {
    let mut records = Vec::new();
    let mut start = 0usize;
    let mut line_index = 1usize;
    while start < new_bytes.len() {
        let end = new_bytes[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(new_bytes.len(), |offset| start + offset + 1);
        let raw = &new_bytes[start..end];
        let content = raw.strip_suffix(b"\n").unwrap_or(raw);
        let content = content.strip_suffix(b"\r").unwrap_or(content);
        let start_offset = base_offset.saturating_add(start as u64);
        let next_offset = base_offset.saturating_add(end as u64);
        match decode_go_observed_event_record(content) {
            Ok(record) => records.push(TailRecord::Event {
                event: decoded_event(record, raw.to_vec()),
                start_offset,
                next_offset,
            }),
            Err(error) => records.push(TailRecord::Corrupt {
                diagnostic: EventScanDiagnostic {
                    line_index,
                    kind: EventScanDiagnosticKind::Corrupt,
                    message: error.to_string(),
                },
                raw_line: raw.to_vec(),
                start_offset,
                next_offset,
            }),
        }
        start = end;
        line_index += 1;
    }
    TailRead {
        records,
        next_offset: base_offset.saturating_add(new_bytes.len() as u64),
        deferred_error: None,
    }
}

/// Lists every `<mission-id>.jsonl` log beneath `root/events`, or an empty
/// list when the directory does not exist yet — matching Go's
/// `runEventsList` treating a missing directory as "no event logs found".
pub fn list_mission_logs(root: &Dir) -> Result<Vec<MissionLogSummary>, EventLogIoError> {
    let Some(events_directory) = open_events_directory(root)? else {
        return Ok(Vec::new());
    };
    let mut summaries = Vec::new();
    let entries = events_directory
        .entries()
        .map_err(|source| EventLogIoError::Io {
            operation: "list events directory",
            source,
        })?;
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(mission_id) = file_name.strip_suffix(".jsonl") else {
            continue;
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        // Re-open no-follow and inspect the opened handle so a replacement
        // between directory enumeration and open cannot escape `events`.
        // Every per-entry failure is skipped, matching Go's independent
        // `entry.Info()` handling while preserving Nanika's stricter
        // symlink boundary.
        let Ok(mut file) = open_file_nofollow_here(&events_directory, file_name) else {
            continue;
        };
        let Ok(metadata) = file.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let event_count = count_non_empty_lines_like_go_scanner(&mut file);
        summaries.push(MissionLogSummary {
            mission_id: mission_id.to_owned(),
            event_count,
            size_bytes: metadata.len(),
        });
    }
    summaries.sort_by(|left, right| left.mission_id.cmp(&right.mission_id));
    Ok(summaries)
}

/// Matches Go's `internal/cmd/events.go` `countLines`: a `bufio.Scanner`
/// with no explicit `Buffer` call, so the default 64 KiB token ceiling
/// applies. A line longer than that ceiling makes `Scan()` return `false`
/// (Go checks `sc.Text()` only inside the loop and never inspects
/// `sc.Err()` afterwards), so counting silently stops at that line rather
/// than erroring or skipping past it. The default split function is
/// `bufio.ScanLines`, whose `dropCR` strips a trailing `\r` from the token
/// *before* the `sc.Text() != ""` emptiness check — so a line consisting of
/// a lone `\r` (from a `\r\n`-only blank line) counts as empty in Go, even
/// though the raw token (used for the length ceiling) still includes it.
fn count_non_empty_lines_like_go_scanner(reader: &mut impl Read) -> u64 {
    let mut count = 0u64;
    let _ignored_scanner_error = scan_lines_like_go(
        reader,
        GO_DEFAULT_SCANNER_MAX_TOKEN_BYTES,
        |content, _, _, _| {
            if !content.is_empty() {
                count += 1;
            }
        },
    );
    count
}

fn open_dir_nofollow_here(start: &Dir, name: &str) -> std::io::Result<Dir> {
    let directory = start.try_clone()?.into_std_file();
    let opened = open_dir_nofollow(&directory, Path::new(name))?;
    Ok(Dir::from_std_file(opened))
}

fn open_file_nofollow_here(directory: &Dir, name: &str) -> std::io::Result<cap_std::fs::File> {
    let mut options = OpenOptions::new();
    options.read(true);
    options._cap_fs_ext_follow(FollowSymlinks::No);
    directory.open_with(Path::new(name), &options)
}

#[cfg(test)]
mod tests {
    use super::{
        EventLogIoError, GO_EVENT_SCANNER_MAX_TOKEN_BYTES, GO_REPLAY_SCANNER_MAX_TOKEN_BYTES,
        MissionId, ReplayRecord, TAIL_BATCH_MAX_RECORDS, TAIL_BATCH_TARGET_BYTES, TailRead,
        TailRecord, last_sequence, last_sequence_in_bytes, list_mission_logs, project_from_log,
        project_mission_in_bytes, replay, replay_reader, replay_records, scan_lines_like_go,
        tail_chunk, tail_reader, tail_since,
    };
    use cap_primitives::fs::open_ambient_dir;
    use cap_std::{ambient_authority, fs::Dir};
    use std::{
        io::{Cursor, Read, Seek, SeekFrom, Write},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    static NONCE: AtomicU64 = AtomicU64::new(1);

    /// A fresh temp directory opened as a `Dir`, removed on drop — matches
    /// the house test convention in `orchestrator-process/src/gate.rs`'s
    /// `Case`.
    struct TestRoot {
        path: PathBuf,
        dir: Dir,
    }

    impl std::ops::Deref for TestRoot {
        type Target = Dir;

        fn deref(&self) -> &Dir {
            &self.dir
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn fresh_root() -> TestResult<TestRoot> {
        let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "orchestrator-exec-event-log-test-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path)?;
        let handle = open_ambient_dir(&path, ambient_authority())?;
        Ok(TestRoot {
            path,
            dir: Dir::from_std_file(handle),
        })
    }

    fn write_log(root: &Dir, mission_id: &str, contents: &str) -> TestResult {
        root.create_dir_all("events")?;
        let mut file = root.create(format!("events/{mission_id}.jsonl"))?;
        file.write_all(contents.as_bytes())?;
        Ok(())
    }

    fn event_line(
        event_type: &str,
        mission_id: &str,
        phase_id: &str,
        seq: i64,
        ts: &str,
    ) -> String {
        let phase = if phase_id.is_empty() {
            String::new()
        } else {
            format!(r#","phase_id":"{phase_id}""#)
        };
        format!(
            r#"{{"id":"evt_{seq:016x}","type":"{event_type}","timestamp":"{ts}","sequence":{seq},"mission_id":"{mission_id}"{phase}}}"#
        )
    }

    fn tail_event_sequences(read: &TailRead) -> Vec<i64> {
        read.records
            .iter()
            .filter_map(|record| match record {
                TailRecord::Event { event, .. } => Some(event.record.sequence),
                TailRecord::Corrupt { .. } => None,
            })
            .collect()
    }

    fn tail_corrupt_raw_lines(read: &TailRead) -> Vec<&[u8]> {
        read.records
            .iter()
            .filter_map(|record| match record {
                TailRecord::Corrupt { raw_line, .. } => Some(raw_line.as_slice()),
                TailRecord::Event { .. } => None,
            })
            .collect()
    }

    struct ReadThenError {
        bytes: Cursor<Vec<u8>>,
        failed: bool,
    }

    impl ReadThenError {
        fn new(bytes: impl Into<Vec<u8>>) -> Self {
            Self {
                bytes: Cursor::new(bytes.into()),
                failed: false,
            }
        }
    }

    impl Read for ReadThenError {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.bytes.position() < self.bytes.get_ref().len() as u64 {
                return self.bytes.read(buffer);
            }
            if self.failed {
                Ok(0)
            } else {
                self.failed = true;
                Err(std::io::Error::other("source-derived fixture read failure"))
            }
        }
    }

    impl Seek for ReadThenError {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.failed = false;
            self.bytes.seek(position)
        }
    }

    // ---- Projection (ported from project_test.go) -------------------------

    #[test]
    fn projection_mission_completed() -> TestResult {
        let bytes = format!(
            "{}\n{}\n",
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z"),
            event_line("mission.completed", "m1", "", 2, "2026-07-13T00:00:01Z"),
        );
        let snap = project_mission_in_bytes(bytes.as_bytes(), "m1")?.ok_or("expected snap")?;
        assert_eq!(snap.status, "completed");
        assert!(!snap.ended_at.is_empty());
        Ok(())
    }

    #[test]
    fn projection_mission_failed() -> TestResult {
        let bytes = format!(
            "{}\n{}\n",
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z"),
            event_line("mission.failed", "m1", "", 2, "2026-07-13T00:00:01Z"),
        );
        let snap = project_mission_in_bytes(bytes.as_bytes(), "m1")?.ok_or("expected snap")?;
        assert_eq!(snap.status, "failed");
        Ok(())
    }

    #[test]
    fn projection_mission_cancelled() -> TestResult {
        let bytes = format!(
            "{}\n{}\n",
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z"),
            event_line("mission.cancelled", "m1", "", 2, "2026-07-13T00:00:01Z"),
        );
        let snap = project_mission_in_bytes(bytes.as_bytes(), "m1")?.ok_or("expected snap")?;
        assert_eq!(snap.status, "cancelled");
        Ok(())
    }

    #[test]
    fn projection_in_progress_when_no_terminal_event() -> TestResult {
        let bytes = event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z") + "\n";
        let snap = project_mission_in_bytes(bytes.as_bytes(), "m1")?.ok_or("expected snap")?;
        assert_eq!(snap.status, "in_progress");
        Ok(())
    }

    #[test]
    fn projection_phase_statuses() -> TestResult {
        let bytes = [
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z"),
            event_line("phase.started", "m1", "p1", 2, "2026-07-13T00:00:01Z"),
            event_line("phase.completed", "m1", "p1", 3, "2026-07-13T00:00:02Z"),
            event_line("phase.started", "m1", "p2", 4, "2026-07-13T00:00:03Z"),
            event_line("phase.failed", "m1", "p2", 5, "2026-07-13T00:00:04Z"),
            event_line("phase.started", "m1", "p3", 6, "2026-07-13T00:00:05Z"),
            event_line("phase.skipped", "m1", "p3", 7, "2026-07-13T00:00:06Z"),
        ]
        .join("\n")
            + "\n";
        let snap = project_mission_in_bytes(bytes.as_bytes(), "m1")?.ok_or("expected snap")?;
        assert_eq!(
            snap.phases.get("p1").ok_or("missing p1")?.status,
            "completed"
        );
        assert_eq!(snap.phases.get("p2").ok_or("missing p2")?.status, "failed");
        assert_eq!(snap.phases.get("p3").ok_or("missing p3")?.status, "skipped");
        Ok(())
    }

    #[test]
    fn projection_phase_retrying() -> TestResult {
        let bytes = [
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z"),
            event_line("phase.started", "m1", "p1", 2, "2026-07-13T00:00:01Z"),
            event_line("phase.retrying", "m1", "p1", 3, "2026-07-13T00:00:02Z"),
        ]
        .join("\n")
            + "\n";
        let snap = project_mission_in_bytes(bytes.as_bytes(), "m1")?.ok_or("expected snap")?;
        assert_eq!(
            snap.phases.get("p1").ok_or("missing p1")?.status,
            "retrying"
        );
        Ok(())
    }

    #[test]
    fn projection_late_join_phase() -> TestResult {
        // Phase event arrives without a prior mission.started — the
        // projection synthesises an in_progress mission, matching Go.
        let bytes = event_line("phase.started", "m1", "p1", 1, "2026-07-13T00:00:00Z") + "\n";
        let snap = project_mission_in_bytes(bytes.as_bytes(), "m1")?.ok_or("expected snap")?;
        assert_eq!(snap.status, "in_progress");
        Ok(())
    }

    #[test]
    fn projection_corrupt_lines_skipped() -> TestResult {
        let good = event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z");
        let bytes = format!("not json\n{good}\n{{bad}}\n");
        let snap = project_mission_in_bytes(bytes.as_bytes(), "m1")?.ok_or("expected snap")?;
        assert_eq!(snap.status, "in_progress");
        Ok(())
    }

    #[test]
    fn projection_unknown_mission_returns_none() -> TestResult {
        let bytes = event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z") + "\n";
        assert!(project_mission_in_bytes(bytes.as_bytes(), "other-mission")?.is_none());
        Ok(())
    }

    #[test]
    fn projection_observes_go_zero_values_in_partial_envelope() -> TestResult {
        let snap =
            project_mission_in_bytes(br#"{"type":"mission.started","mission_id":"m1"}"#, "m1")?
                .ok_or("expected snapshot")?;
        assert_eq!(snap.started_at, orchestrator_core::GO_ZERO_TIME);
        Ok(())
    }

    // ---- last_sequence (ported from event_test.go's TestLastSequence) -----

    #[test]
    fn last_sequence_missing_file_is_zero() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("m1")?;
        assert_eq!(last_sequence(&root, &mission)?, 0);
        Ok(())
    }

    #[test]
    fn last_sequence_returns_greatest_sequence() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("m1")?;
        let contents = [
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z"),
            event_line("mission.started", "m1", "", 2, "2026-07-13T00:00:01Z"),
            event_line("mission.started", "m1", "", 3, "2026-07-13T00:00:02Z"),
        ]
        .join("\n")
            + "\n";
        write_log(&root, "m1", &contents)?;
        assert_eq!(last_sequence(&root, &mission)?, 3);
        Ok(())
    }

    #[test]
    fn last_sequence_negative_only_log_is_zero() -> TestResult {
        let bytes = [
            event_line("mission.started", "m1", "", -2, "2026-07-13T00:00:00Z"),
            event_line("mission.started", "m1", "", -1, "2026-07-13T00:00:01Z"),
        ]
        .join("\n")
            + "\n";
        assert_eq!(last_sequence_in_bytes(bytes.as_bytes())?, 0);
        Ok(())
    }

    #[test]
    fn last_sequence_observes_partial_go_envelope() -> TestResult {
        assert_eq!(last_sequence_in_bytes(br#"{"sequence":7}"#)?, 7);
        Ok(())
    }

    // ---- ProjectFromLog (file-backed) --------------------------------------

    #[test]
    fn project_from_log_no_log_returns_none() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("nonexistent")?;
        assert!(project_from_log(&root, &mission)?.is_none());
        Ok(())
    }

    #[test]
    fn project_from_log_uses_events_directory() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("m1")?;
        let contents = [
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z"),
            event_line("mission.completed", "m1", "", 2, "2026-07-13T00:00:01Z"),
        ]
        .join("\n")
            + "\n";
        write_log(&root, "m1", &contents)?;
        let snap = project_from_log(&root, &mission)?.ok_or("expected snapshot")?;
        assert_eq!(snap.status, "completed");
        Ok(())
    }

    #[test]
    fn project_from_log_phase_count_matches_events() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("m1")?;
        let contents = [
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z"),
            event_line("phase.started", "m1", "p1", 2, "2026-07-13T00:00:01Z"),
            event_line("phase.completed", "m1", "p1", 3, "2026-07-13T00:00:02Z"),
            event_line("phase.started", "m1", "p2", 4, "2026-07-13T00:00:03Z"),
            event_line("phase.completed", "m1", "p2", 5, "2026-07-13T00:00:04Z"),
            event_line("mission.completed", "m1", "", 6, "2026-07-13T00:00:05Z"),
        ]
        .join("\n")
            + "\n";
        write_log(&root, "m1", &contents)?;
        let snap = project_from_log(&root, &mission)?.ok_or("expected snapshot")?;
        assert_eq!(snap.status, "completed");
        let completed = snap
            .phases
            .values()
            .filter(|phase| phase.status == "completed")
            .count();
        assert_eq!(completed, 2);
        Ok(())
    }

    // ---- list/tail (read-only core of internal/cmd/events.go) -------------

    #[test]
    fn list_mission_logs_empty_directory_is_empty() -> TestResult {
        let root = fresh_root()?;
        assert!(list_mission_logs(&root)?.is_empty());
        Ok(())
    }

    #[test]
    fn list_mission_logs_reports_count_and_size() -> TestResult {
        let root = fresh_root()?;
        let contents = [
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z"),
            event_line("mission.completed", "m1", "", 2, "2026-07-13T00:00:01Z"),
        ]
        .join("\n")
            + "\n";
        write_log(&root, "m1", &contents)?;
        let summaries = list_mission_logs(&root)?;
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].mission_id, "m1");
        assert_eq!(summaries[0].event_count, 2);
        assert_eq!(summaries[0].size_bytes, contents.len() as u64);
        Ok(())
    }

    #[test]
    fn tail_chunk_emits_unterminated_valid_eof_record() {
        let line = event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z");
        let chunk = tail_chunk(line.as_bytes(), 0);
        assert_eq!(tail_event_sequences(&chunk), vec![1]);
        assert!(tail_corrupt_raw_lines(&chunk).is_empty());
        assert_eq!(chunk.next_offset, line.len() as u64);
        assert!(chunk.deferred_error.is_none());
    }

    #[test]
    fn tail_chunk_emits_unterminated_corrupt_eof_bytes() {
        let chunk = tail_chunk(b"not json", 17);
        assert_eq!(tail_corrupt_raw_lines(&chunk), vec![b"not json".as_slice()]);
        assert_eq!(chunk.next_offset, 25);
        assert_eq!(chunk.records[0].start_offset(), 17);
        assert_eq!(chunk.records[0].next_offset(), 25);
    }

    #[test]
    fn tail_since_advances_across_two_reads() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("m1")?;
        let first = event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z") + "\n";
        write_log(&root, "m1", &first)?;

        let read1 = tail_since(&root, &mission, 0)?;
        assert_eq!(tail_event_sequences(&read1), vec![1]);
        assert_eq!(read1.next_offset, first.len() as u64);

        let second = event_line("mission.completed", "m1", "", 2, "2026-07-13T00:00:01Z") + "\n";
        let full = format!("{first}{second}");
        write_log(&root, "m1", &full)?;

        let read2 = tail_since(&root, &mission, read1.next_offset)?;
        assert_eq!(tail_event_sequences(&read2), vec![2]);
        assert_eq!(read2.next_offset, full.len() as u64);
        Ok(())
    }

    #[test]
    fn tail_since_batches_many_records_without_loss() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("m1")?;
        let expected = TAIL_BATCH_MAX_RECORDS + 17;
        let mut contents = String::new();
        for sequence in 1..=expected {
            contents.push_str(&event_line(
                "mission.started",
                "m1",
                "",
                sequence as i64,
                "2026-07-13T00:00:00Z",
            ));
            contents.push('\n');
        }
        write_log(&root, "m1", &contents)?;

        let mut offset = 0u64;
        let mut sequences = Vec::new();
        let mut batches = 0usize;
        while offset < contents.len() as u64 {
            let batch = tail_since(&root, &mission, offset)?;
            assert!(batch.next_offset > offset);
            assert!(batch.records.len() <= TAIL_BATCH_MAX_RECORDS);
            assert!(tail_corrupt_raw_lines(&batch).is_empty());
            assert!(batch.deferred_error.is_none());
            sequences.extend(tail_event_sequences(&batch));
            offset = batch.next_offset;
            batches += 1;
        }
        assert_eq!(sequences, (1..=expected as i64).collect::<Vec<_>>());
        assert!(batches >= 2);
        Ok(())
    }

    #[test]
    fn tail_since_allows_one_record_larger_than_batch_byte_target() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("m1")?;
        let padding = "x".repeat(TAIL_BATCH_TARGET_BYTES * 2);
        let first = format!(
            "{}\n",
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z").replacen(
                "}",
                &format!(",\"data\":{{\"padding\":\"{padding}\"}}}}"),
                1
            )
        );
        let second = event_line("mission.completed", "m1", "", 2, "2026-07-13T00:00:01Z") + "\n";
        write_log(&root, "m1", &(first.clone() + &second))?;

        let batch = tail_since(&root, &mission, 0)?;
        assert_eq!(tail_event_sequences(&batch), vec![1]);
        assert_eq!(batch.next_offset, first.len() as u64);
        let continuation = tail_since(&root, &mission, batch.next_offset)?;
        assert_eq!(tail_event_sequences(&continuation), vec![2]);
        Ok(())
    }

    #[test]
    fn tail_since_consumes_partial_eof_before_later_newline() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("m1")?;
        let first = br#"{"sequence":7}"#;
        root.create_dir_all("events")?;
        let mut file = root.create("events/m1.jsonl")?;
        file.write_all(first)?;
        drop(file);

        let partial = tail_since(&root, &mission, 0)?;
        assert_eq!(tail_event_sequences(&partial), vec![7]);
        assert_eq!(partial.next_offset, first.len() as u64);

        let mut file = root.create("events/m1.jsonl")?;
        file.write_all(first)?;
        file.write_all(b"\n{\"sequence\":8}\n")?;
        drop(file);
        let continuation = tail_since(&root, &mission, partial.next_offset)?;
        assert!(matches!(
            &continuation.records[..],
            [
                TailRecord::Corrupt { raw_line, .. },
                TailRecord::Event { event, .. }
            ] if raw_line == b"\n" && event.record.sequence == 8
        ));
        Ok(())
    }

    #[test]
    fn tail_since_missing_mission_is_not_found() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("nonexistent")?;
        match tail_since(&root, &mission, 0) {
            Err(EventLogIoError::NotFound) => Ok(()),
            other => Err(format!("expected NotFound, got {other:?}").into()),
        }
    }

    // ---- oversized-line abort semantics (review-fix regression coverage) --

    /// Go's `bufio.Scanner.Buffer(make([]byte, 1<<20), 1<<20)` overflows at
    /// content strictly greater than 1 MiB - 1 byte
    /// (`orchestrator_core::GO_EVENT_JSON_CONTENT_MAX_BYTES`).
    fn oversized_line() -> String {
        let padding = "x".repeat(orchestrator_core::GO_EVENT_JSON_CONTENT_MAX_BYTES + 1);
        format!(r#"{{"padding":"{padding}"}}"#)
    }

    #[test]
    fn last_sequence_in_bytes_aborts_on_oversized_line() -> TestResult {
        // seq=1 (valid), then an oversized line, then seq=99 (valid) —
        // Go's LastSequence returns (1, ErrTooLong): the caller never sees
        // 99 because scanner.Err() is non-nil and the whole read is treated
        // as failed, not a partial 1.
        let bytes = format!(
            "{}\n{}\n{}\n",
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z"),
            oversized_line(),
            event_line("mission.started", "m1", "", 99, "2026-07-13T00:00:02Z"),
        );
        match last_sequence_in_bytes(bytes.as_bytes()) {
            Err(EventLogIoError::OversizedLine) => Ok(()),
            other => Err(format!("expected OversizedLine, got {other:?}").into()),
        }
    }

    #[test]
    fn last_sequence_rejects_token_that_fills_scanner_buffer() -> TestResult {
        let mut bytes = vec![b'x'; GO_EVENT_SCANNER_MAX_TOKEN_BYTES];
        bytes.push(b'\n');
        match last_sequence_in_bytes(&bytes) {
            Err(EventLogIoError::OversizedLine) => Ok(()),
            other => Err(format!("expected OversizedLine, got {other:?}").into()),
        }
    }

    #[test]
    fn project_mission_in_bytes_aborts_on_oversized_line() -> TestResult {
        let bytes = format!(
            "{}\n{}\n{}\n",
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z"),
            oversized_line(),
            event_line("mission.completed", "m1", "", 99, "2026-07-13T00:00:02Z"),
        );
        match project_mission_in_bytes(bytes.as_bytes(), "m1") {
            Err(EventLogIoError::OversizedLine) => Ok(()),
            other => Err(format!("expected OversizedLine, got {other:?}").into()),
        }
    }

    #[test]
    fn project_from_log_aborts_on_oversized_line() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("m1")?;
        let contents = format!(
            "{}\n{}\n",
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z"),
            oversized_line(),
        );
        write_log(&root, "m1", &contents)?;
        match project_from_log(&root, &mission) {
            Err(EventLogIoError::OversizedLine) => Ok(()),
            other => Err(format!("expected OversizedLine, got {other:?}").into()),
        }
    }

    #[test]
    fn last_sequence_just_under_the_ceiling_succeeds() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("m1")?;
        let template = r#"{"id":"evt_1","type":"mission.started","timestamp":"2026-07-13T00:00:00Z","sequence":1,"mission_id":"m1","data":{"padding":""}}"#;
        let padding =
            "x".repeat(orchestrator_core::GO_EVENT_JSON_CONTENT_MAX_BYTES - template.len());
        let big_but_ok = template.replacen("\"\"", &format!("\"{padding}\""), 1);
        assert!(big_but_ok.len() <= orchestrator_core::GO_EVENT_JSON_CONTENT_MAX_BYTES);
        write_log(&root, "m1", &(big_but_ok + "\n"))?;
        assert_eq!(last_sequence(&root, &mission)?, 1);
        Ok(())
    }

    // ---- replay: 64 KiB ceiling, distinct from the 1 MiB LastSequence/
    // ProjectFromLog ceiling -----------------------------------------------

    #[test]
    fn replay_aborts_on_a_line_over_64kib_even_though_under_1mib() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("m1")?;
        // 70 KiB of content: over Go's replay scanner's 64 KiB buffer, but
        // nowhere near the 1 MiB LastSequence/ProjectFromLog ceiling — this
        // line would be decoded fine by `last_sequence`/`project_from_log`,
        // but must abort `replay`.
        let padding = "x".repeat(70 * 1024);
        let mid_line = format!(r#"{{"padding":"{padding}"}}"#);
        let contents = format!(
            "{}\n{}\n",
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z"),
            mid_line,
        );
        write_log(&root, "m1", &contents)?;

        // Confirm the same bytes do NOT trip the 1 MiB ceiling used by
        // last_sequence/project_from_log.
        assert_eq!(last_sequence(&root, &mission)?, 1);

        match replay(&root, &mission) {
            Err(EventLogIoError::OversizedLine) => Ok(()),
            other => Err(format!("expected OversizedLine, got {other:?}").into()),
        }
    }

    #[test]
    fn replay_rejects_token_that_fills_scanner_buffer() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("m1")?;
        let mut contents = vec![b'x'; GO_REPLAY_SCANNER_MAX_TOKEN_BYTES];
        contents.push(b'\n');
        root.create_dir_all("events")?;
        let mut file = root.create("events/m1.jsonl")?;
        file.write_all(&contents)?;

        match replay(&root, &mission) {
            Err(EventLogIoError::OversizedLine) => Ok(()),
            other => Err(format!("expected OversizedLine, got {other:?}").into()),
        }
    }

    #[test]
    fn file_readers_stream_logs_larger_than_legacy_total_cap() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("m1")?;
        root.create_dir_all("events")?;
        let mut file = root.create("events/m1.jsonl")?;
        let largest_replay_token = vec![b'x'; GO_REPLAY_SCANNER_MAX_TOKEN_BYTES - 1];
        for _ in 0..1025 {
            file.write_all(&largest_replay_token)?;
            file.write_all(b"\n")?;
        }
        let final_event = event_line("mission.started", "m1", "", 7, "2026-07-13T00:00:00Z");
        file.write_all(final_event.as_bytes())?;
        file.write_all(b"\n")?;
        drop(file);

        assert_eq!(last_sequence(&root, &mission)?, 7);
        assert_eq!(
            project_from_log(&root, &mission)?
                .ok_or("expected projection")?
                .status,
            "in_progress"
        );
        let (events, diagnostics) = replay(&root, &mission)?;
        assert_eq!(events.len(), 1);
        assert_eq!(diagnostics.len(), 1025);
        Ok(())
    }

    #[test]
    fn replay_returns_events_in_file_order() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("m1")?;
        let contents = [
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z"),
            event_line("phase.started", "m1", "p1", 2, "2026-07-13T00:00:01Z"),
            event_line("mission.completed", "m1", "", 3, "2026-07-13T00:00:02Z"),
        ]
        .join("\n")
            + "\n";
        write_log(&root, "m1", &contents)?;
        let (events, diagnostics) = replay(&root, &mission)?;
        assert_eq!(events.len(), 3);
        assert!(diagnostics.is_empty());
        assert_eq!(events[0].record.sequence, 1);
        assert_eq!(events[2].record.sequence, 3);
        Ok(())
    }

    #[test]
    fn replay_records_yields_partial_events_and_corruption_in_file_order() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("m1")?;
        write_log(
            &root,
            "m1",
            "{\"sequence\":7}\nnot json\n{\"sequence\":8}\n",
        )?;

        let records = replay_records(&root, &mission)?.collect::<Result<Vec<_>, _>>()?;
        assert_eq!(records.len(), 3);
        assert!(matches!(
            &records[0],
            ReplayRecord::Event(event) if event.record.sequence == 7
                && event.record.timestamp == orchestrator_core::GO_ZERO_TIME
        ));
        assert!(matches!(
            &records[1],
            ReplayRecord::Corrupt { raw_line, .. } if raw_line == b"not json\n"
        ));
        assert!(matches!(
            &records[2],
            ReplayRecord::Event(event) if event.record.sequence == 8
        ));
        Ok(())
    }

    #[test]
    fn replay_records_is_lazy_before_a_later_oversized_line() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("m1")?;
        root.create_dir_all("events")?;
        let mut file = root.create("events/m1.jsonl")?;
        file.write_all(b"{\"sequence\":7}\n")?;
        file.write_all(&vec![b'x'; GO_REPLAY_SCANNER_MAX_TOKEN_BYTES])?;
        file.write_all(b"\n")?;
        drop(file);

        let mut records = replay_records(&root, &mission)?;
        assert!(matches!(
            records.next(),
            Some(Ok(ReplayRecord::Event(event))) if event.record.sequence == 7
        ));
        assert!(matches!(
            records.next(),
            Some(Err(EventLogIoError::OversizedLine))
        ));
        assert!(records.next().is_none());
        Ok(())
    }

    #[test]
    fn replay_yields_pending_fragment_before_a_deferred_read_error() -> TestResult {
        let mut records = replay_reader(ReadThenError::new(
            b"{\"sequence\":7}\n{\"sequence\":8}".as_slice(),
        ));
        assert!(matches!(
            records.next(),
            Some(Ok(ReplayRecord::Event(event))) if event.record.sequence == 7
        ));
        assert!(matches!(
            records.next(),
            Some(Ok(ReplayRecord::Event(event)))
                if event.record.sequence == 8 && event.raw_line == b"{\"sequence\":8}"
        ));
        assert!(matches!(
            records.next(),
            Some(Err(EventLogIoError::Io { .. }))
        ));
        assert!(records.next().is_none());
        Ok(())
    }

    #[test]
    fn non_owning_scan_visits_pending_fragment_before_returning_read_error() {
        let mut reader = ReadThenError::new(b"first\nsecond".as_slice());
        let mut seen = Vec::new();
        let result = scan_lines_like_go(&mut reader, 64, |content, _, _, _| {
            seen.push(content.to_vec());
        });
        assert!(matches!(result, Err(EventLogIoError::Io { .. })));
        assert_eq!(seen, vec![b"first".to_vec(), b"second".to_vec()]);
    }

    #[test]
    fn tail_batch_orders_records_then_carries_deferred_read_error() -> TestResult {
        let input = b"{\"sequence\":7}\nnot json\n{\"sequence\":8}";
        let read = tail_reader(ReadThenError::new(input.as_slice()), 0)?;
        assert!(matches!(
            &read.records[..],
            [
                TailRecord::Event { event: first, .. },
                TailRecord::Corrupt { raw_line, .. },
                TailRecord::Event { event: last, .. }
            ] if first.record.sequence == 7
                && raw_line == b"not json\n"
                && last.record.sequence == 8
        ));
        assert_eq!(
            read.records
                .iter()
                .map(TailRecord::start_offset)
                .collect::<Vec<_>>(),
            vec![0, 15, 24]
        );
        assert_eq!(read.records.last().map(TailRecord::next_offset), Some(38));
        assert_eq!(read.next_offset, input.len() as u64);
        assert!(matches!(
            read.deferred_error.as_ref(),
            Some(EventLogIoError::Io { .. })
        ));
        let mut ordered = read.into_iter();
        assert!(matches!(
            ordered.next(),
            Some(Ok(TailRecord::Event { event, .. })) if event.record.sequence == 7
        ));
        assert!(matches!(
            ordered.next(),
            Some(Ok(TailRecord::Corrupt { raw_line, .. })) if raw_line == b"not json\n"
        ));
        assert!(matches!(
            ordered.next(),
            Some(Ok(TailRecord::Event { event, .. })) if event.record.sequence == 8
        ));
        assert!(matches!(
            ordered.next(),
            Some(Err(EventLogIoError::Io { .. }))
        ));
        assert!(ordered.next().is_none());
        Ok(())
    }

    #[test]
    fn tail_returns_immediate_error_when_no_record_precedes_it() {
        assert!(matches!(
            tail_reader(ReadThenError::new(Vec::<u8>::new()), 0),
            Err(EventLogIoError::Io { .. })
        ));
    }

    #[test]
    fn direct_file_reader_adapters_keep_replay_and_tail_bounded_semantics() -> TestResult {
        let root = fresh_root()?;
        let first = b"{\"sequence\":7}\n";
        let second = b"{\"SEQUENCE\":8}\n";
        let mut contents = Vec::from(first);
        contents.extend_from_slice(second);
        write_log(&root, "direct", std::str::from_utf8(&contents)?)?;
        let path = root.path.join("events/direct.jsonl");

        let replayed = replay_reader(std::fs::File::open(&path)?)
            .collect::<Result<Vec<_>, EventLogIoError>>()?;
        assert_eq!(replayed.len(), 2);
        assert!(matches!(
            &replayed[0],
            ReplayRecord::Event(event) if event.record.sequence == 7
        ));
        assert!(matches!(
            &replayed[1],
            ReplayRecord::Event(event) if event.record.sequence == 8
        ));

        let tailed = tail_reader(std::fs::File::open(path)?, first.len() as u64)?;
        assert_eq!(tail_event_sequences(&tailed), vec![8]);
        assert_eq!(tailed.next_offset, contents.len() as u64);
        Ok(())
    }

    #[test]
    fn replay_missing_mission_is_not_found() -> TestResult {
        let root = fresh_root()?;
        let mission = MissionId::new("nonexistent")?;
        match replay(&root, &mission) {
            Err(EventLogIoError::NotFound) => Ok(()),
            other => Err(format!("expected NotFound, got {other:?}").into()),
        }
    }

    // ---- list_mission_logs: per-entry independence -------------------------

    #[test]
    fn list_mission_logs_scanner_overflow_does_not_hide_other_entries() -> TestResult {
        let root = fresh_root()?;
        let good = [
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z"),
            event_line("mission.completed", "m1", "", 2, "2026-07-13T00:00:01Z"),
        ]
        .join("\n")
            + "\n";
        write_log(&root, "m1", &good)?;

        root.create_dir_all("events")?;
        {
            let mut oversized = root.create("events/m2.jsonl")?;
            oversized.write_all(&vec![b'x'; GO_REPLAY_SCANNER_MAX_TOKEN_BYTES])?;
            oversized.write_all(b"\nnonempty-after-overflow\n")?;
        }

        let summaries = list_mission_logs(&root)?;
        assert_eq!(summaries.len(), 2);
        let m1 = summaries
            .iter()
            .find(|summary| summary.mission_id == "m1")
            .ok_or("missing m1")?;
        assert_eq!(m1.event_count, 2);
        let m2 = summaries
            .iter()
            .find(|summary| summary.mission_id == "m2")
            .ok_or("missing m2")?;
        assert_eq!(m2.event_count, 0);
        Ok(())
    }

    #[test]
    fn non_owning_scans_reuse_one_line_allocation() -> TestResult {
        let mut reader = Cursor::new(b"first\nsecond\nthird\n");
        let mut token_addresses = Vec::new();
        scan_lines_like_go(&mut reader, 64, |content, _, _, _| {
            token_addresses.push(content.as_ptr() as usize);
        })?;
        assert_eq!(token_addresses.len(), 3);
        assert!(token_addresses.windows(2).all(|pair| pair[0] == pair[1]));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn list_mission_logs_skips_symlinks_without_hiding_regular_entries() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = fresh_root()?;
        let good = event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z") + "\n";
        write_log(&root, "m1", &good)?;
        std::fs::write(root.path.join("outside.jsonl"), &good)?;
        symlink(
            root.path.join("outside.jsonl"),
            root.path.join("events/linked.jsonl"),
        )?;
        symlink(
            root.path.join("missing.jsonl"),
            root.path.join("events/dangling.jsonl"),
        )?;

        let summaries = list_mission_logs(&root)?;
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].mission_id, "m1");
        Ok(())
    }

    // ---- countLines \r\n blank-line parity ---------------------------------

    #[test]
    fn list_mission_logs_blank_crlf_line_is_not_counted() -> TestResult {
        let root = fresh_root()?;
        let contents = format!(
            "{}\r\n\r\n{}\n",
            event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z"),
            event_line("mission.completed", "m1", "", 2, "2026-07-13T00:00:01Z"),
        );
        write_log(&root, "m1", &contents)?;
        let summaries = list_mission_logs(&root)?;
        assert_eq!(summaries.len(), 1);
        // 2 real event lines + 1 blank CRLF-only line that Go's dropCR
        // treats as empty and excludes.
        assert_eq!(summaries[0].event_count, 2);
        Ok(())
    }

    // ---- tail_chunk: raw bytes retained for diagnostics --------------------

    #[test]
    fn tail_chunk_retains_raw_bytes_for_a_corrupt_line() {
        let good = event_line("mission.started", "m1", "", 1, "2026-07-13T00:00:00Z");
        let input = format!("{good}\nnot json\n");
        let chunk = tail_chunk(input.as_bytes(), 0);
        assert_eq!(tail_event_sequences(&chunk), vec![1]);
        assert_eq!(
            tail_corrupt_raw_lines(&chunk),
            vec![b"not json\n".as_slice()]
        );
    }
}
