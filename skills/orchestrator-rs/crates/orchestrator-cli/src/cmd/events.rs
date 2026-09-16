//! Ports Go's `orchestrator events {list,replay,tail}` (`internal/cmd/events.go`).
//!
//! `list` remains a compatibility adapter over plain `std::fs`. `replay` and
//! `tail` validate and open the selected path through the read-only app
//! capability, then transfer that file to `orchestrator_exec`'s bounded,
//! ordered reader APIs. This preserves corrupt-line source order and raw-byte
//! fallback without collecting a whole log in memory.

use std::{
    io::{Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use orchestrator_app::{ReadOnlyEventLogAuthority, ReadOnlyEventLogTarget};
use orchestrator_core::{EventRecord, decode_event_line};
use orchestrator_daemon::{
    DaemonClient, DaemonError, DaemonSubscription, DaemonSubscriptionCancel,
};
use orchestrator_exec::{
    EventLogIoError, ReplayRecord, TailRead, TailRecord, replay_reader, tail_reader,
};
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    iterator::{Handle as SignalHandle, Signals},
};
use thiserror::Error;

use super::time_fmt::hms_millis_from_rfc3339;

/// Matches Go's default untuned scanner used by `countLines` (list).
/// `orchestrator_exec::replay_reader` independently owns replay's explicit,
/// equal-sized scanner ceiling.
const SCANNER_MAX_TOKEN_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
pub(crate) enum EventsError {
    #[error("resolving events dir: {0}")]
    ReadEventsDir(#[source] std::io::Error),
    #[error("no event log found for mission {mission_id:?} (looked at {path})")]
    NotFound { mission_id: String, path: String },
    #[error("event log path for mission {mission_id:?} escapes the runtime events directory")]
    UnsafePath { mission_id: String },
    #[error("opening event log: open {path}: {message}")]
    OpenLog {
        path: String,
        message: String,
        #[source]
        source: std::io::Error,
    },
    #[error("opening log file: open {path}: {message}")]
    OpenTailLog {
        path: String,
        message: String,
        #[source]
        source: std::io::Error,
    },
    #[error("log file {path} did not appear within {timeout_seconds}s")]
    TailLogWaitTimeout { path: String, timeout_seconds: u64 },
    #[error("seeking to end of log: seek {path}: {message}")]
    SeekLog {
        path: String,
        message: String,
        #[source]
        source: std::io::Error,
    },
    #[error("reading log: {0}")]
    ReadLog(#[source] std::io::Error),
    #[error("cannot write command output: {0}")]
    Output(#[from] std::io::Error),
}

// ---- list -----------------------------------------------------------------

struct LogSummary {
    mission_id: String,
    event_count: u64,
    size_bytes: u64,
}

/// Ports `runEventsList` (`internal/cmd/events.go:66-113`).
pub(crate) fn run_list(home: &Path, output: &mut impl Write) -> Result<(), EventsError> {
    let events_dir = home.join("events");
    let entries = match std::fs::read_dir(&events_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            writeln!(output, "no event logs found (directory does not exist yet)")?;
            return Ok(());
        }
        Err(error) => return Err(EventsError::ReadEventsDir(error)),
    };

    let mut summaries = Vec::new();
    for entry in entries {
        let entry = entry.map_err(EventsError::ReadEventsDir)?;
        let file_type = entry.file_type().map_err(EventsError::ReadEventsDir)?;
        if !file_type.is_file() {
            continue;
        }
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(mission_id) = file_name.strip_suffix(".jsonl") else {
            continue;
        };
        let size_bytes = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        let event_count = count_lines_like_go_scanner(&entry.path());
        summaries.push(LogSummary {
            mission_id: mission_id.to_owned(),
            event_count,
            size_bytes,
        });
    }

    if summaries.is_empty() {
        writeln!(output, "no event logs found")?;
        return Ok(());
    }

    // Go prints newest-first by reversing the lexicographically-sorted
    // `os.ReadDir` order; sort ascending here and iterate in reverse to match.
    summaries.sort_by(|a, b| a.mission_id.cmp(&b.mission_id));

    writeln!(output, "{:<30}  {:>8}  SIZE", "MISSION ID", "EVENTS")?;
    writeln!(output, "{}", "-".repeat(55))?;
    for summary in summaries.iter().rev() {
        writeln!(
            output,
            "{:<30}  {:>8}  {}",
            summary.mission_id,
            summary.event_count,
            human_size(summary.size_bytes),
        )?;
    }
    Ok(())
}

/// Ports `countLines` (`internal/cmd/events.go:327-342`): counts non-empty
/// lines with `bufio.Scanner`'s default 64 KiB token ceiling. A line over
/// the ceiling makes `Scan()` return `false` permanently — Go never checks
/// `sc.Err()` here, so counting silently stops at that line. `\r\n`-only
/// blank lines count as empty (`dropCR` strips the trailing `\r` before the
/// emptiness check).
fn count_lines_like_go_scanner(path: &Path) -> u64 {
    let Ok(bytes) = std::fs::read(path) else {
        return 0;
    };
    let mut count = 0u64;
    let mut start = 0usize;
    while start < bytes.len() {
        let end = bytes[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |offset| start + offset);
        let raw_line = &bytes[start..end];
        if raw_line.len() > SCANNER_MAX_TOKEN_BYTES {
            break;
        }
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if !line.is_empty() {
            count += 1;
        }
        start = if end < bytes.len() { end + 1 } else { end };
    }
    count
}

/// Ports `humanSize` (`internal/cmd/events.go:345-354`).
fn human_size(bytes: u64) -> String {
    if bytes >= 1 << 20 {
        format!("{:.1} MB", bytes as f64 / f64::from(1u32 << 20))
    } else if bytes >= 1 << 10 {
        format!("{:.1} KB", bytes as f64 / f64::from(1u32 << 10))
    } else {
        format!("{bytes} B")
    }
}

// ---- replay -----------------------------------------------------------------

struct ResolvedLog {
    path: PathBuf,
    target: ReadOnlyEventLogTarget,
    daemon_stream_eligible: bool,
}

#[derive(Clone, Copy)]
enum LogUse {
    Replay,
    Tail,
}

impl LogUse {
    fn open_error(self, path: &Path, source: std::io::Error) -> EventsError {
        match self {
            Self::Replay => open_log_error(path, source),
            Self::Tail => open_tail_log_error(path, source),
        }
    }
}

/// Ports `resolveLogPath` (`internal/cmd/events.go:209-228`) while binding the
/// selected entry to a retained, no-follow read capability. A direct path keeps
/// Go's original spelling. Mission fallback uses `filepath.Join`/`Clean`-shaped
/// lexical normalization, but rejects parent traversal that would escape the
/// runtime home's `events` directory.
fn resolve_log_target(
    home: &Path,
    argument: &str,
    log_use: LogUse,
) -> Result<ResolvedLog, EventsError> {
    let authority = ReadOnlyEventLogAuthority::acquire_ambient()
        .map_err(|source| log_use.open_error(Path::new(argument), source))?;
    if argument.ends_with(".jsonl") {
        let candidate = PathBuf::from(argument);
        if let Ok(target) = authority.bind_direct(&candidate) {
            if target.probe().is_ok() {
                return Ok(ResolvedLog {
                    path: candidate,
                    target,
                    daemon_stream_eligible: false,
                });
            }
        }
    }

    let home = clean_path_like_go(home);
    let relative = clean_fallback_relative(argument)?;
    let path = home.join("events").join(&relative);
    let not_found = || EventsError::NotFound {
        mission_id: argument.to_owned(),
        path: path.display().to_string(),
    };

    let root = match authority.bind_runtime_home(&home) {
        Ok(root) => root,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Err(not_found()),
        Err(source) => return Err(log_use.open_error(&path, source)),
    };
    let target = match root.event_log(&Path::new("events").join(&relative)) {
        Ok(target) => target,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Err(not_found()),
        Err(source) => return Err(log_use.open_error(&path, source)),
    };
    match target.probe() {
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Err(not_found()),
        Ok(()) | Err(_) => Ok(ResolvedLog {
            path,
            target,
            daemon_stream_eligible: true,
        }),
    }
}

fn clean_fallback_relative(argument: &str) -> Result<PathBuf, EventsError> {
    let log_name = format!("{argument}.jsonl");
    let mut relative = PathBuf::new();
    for component in Path::new(&log_name).components() {
        match component {
            Component::CurDir | Component::RootDir => {}
            Component::Normal(name) => relative.push(name),
            Component::ParentDir => {
                if !relative.pop() {
                    return Err(EventsError::UnsafePath {
                        mission_id: argument.to_owned(),
                    });
                }
            }
            Component::Prefix(_) => {
                return Err(EventsError::UnsafePath {
                    mission_id: argument.to_owned(),
                });
            }
        }
    }
    Ok(relative)
}

fn clean_path_like_go(path: &Path) -> PathBuf {
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let last_is_normal =
                    matches!(cleaned.components().next_back(), Some(Component::Normal(_)));
                if last_is_normal {
                    cleaned.pop();
                } else if !cleaned.has_root() {
                    cleaned.push(component.as_os_str());
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                cleaned.push(component.as_os_str());
            }
        }
    }
    cleaned
}

/// Ports `runEventsReplay` (`internal/cmd/events.go:117-132`).
pub(crate) fn run_replay(
    home: &Path,
    mission_id_argument: &str,
    raw_json: bool,
    output: &mut impl Write,
) -> Result<(), EventsError> {
    let resolved = resolve_log_target(home, mission_id_argument, LogUse::Replay)?;
    let file = resolved
        .target
        .open()
        .map_err(|source| open_log_error(&resolved.path, source))?;
    for record in replay_reader(file) {
        match record.map_err(event_log_read_error)? {
            ReplayRecord::Event(event) => {
                if raw_json {
                    write_replay_raw_line(output, &event.raw_line)?;
                } else {
                    format_event_line(&event.record, output)?;
                }
            }
            ReplayRecord::Corrupt { raw_line, .. } => {
                write_replay_raw_line(output, &raw_line)?;
            }
        }
    }
    Ok(())
}

/// Go replay prints `Scanner.Text()`, whose `ScanLines` split drops one final
/// carriage return from both newline-delimited and unterminated EOF tokens.
fn write_replay_raw_line(output: &mut impl Write, raw_line: &[u8]) -> Result<(), EventsError> {
    let token = raw_line.strip_suffix(b"\n").unwrap_or(raw_line);
    let token = token.strip_suffix(b"\r").unwrap_or(token);
    output.write_all(token)?;
    output.write_all(b"\n")?;
    Ok(())
}

/// Go tail trims the newline returned by `ReadString`, but retains `\r`.
fn write_tail_raw_line(output: &mut impl Write, raw_line: &[u8]) -> Result<(), EventsError> {
    let line = raw_line.strip_suffix(b"\n").unwrap_or(raw_line);
    output.write_all(line)?;
    output.write_all(b"\n")?;
    Ok(())
}

/// Ports `formatEventLine` (`internal/cmd/events.go:273-306`). Data-map
/// context entries iterate in `EventJsonMap` (sorted-key) order rather than
/// Go's random `map` iteration order — an inherited, deterministic
/// improvement over Go's own unspecified order (`orchestrator_core`'s
/// `EventRecord.data` is already an `EventJsonMap`; see `PORTING.md` §3's
/// map-iteration-order guidance), not a new divergence introduced here.
fn format_event_line(record: &EventRecord, output: &mut impl Write) -> Result<(), EventsError> {
    let timestamp = hms_millis_from_rfc3339(&record.timestamp);
    let colour = event_colour(&record.event_type);
    const RESET: &str = "\x1b[0m";

    let mut parts = Vec::new();
    if let Some(phase_id) = record.phase_id.as_deref().filter(|value| !value.is_empty()) {
        parts.push(format!("phase={phase_id}"));
    }
    if let Some(worker_id) = record
        .worker_id
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        parts.push(format!("worker={worker_id}"));
    }
    if let Some(data) = &record.data {
        for (key, value) in data.iter() {
            parts.push(format!("{key}={}", go_display_value(value)));
        }
    }
    let context = if parts.is_empty() {
        String::new()
    } else {
        format!("  {}", parts.join(" "))
    };

    writeln!(
        output,
        "{timestamp} {:>4} {colour}{:<30}{RESET}{context}",
        record.sequence, record.event_type,
    )?;
    Ok(())
}

/// Ports `eventColour` (`internal/cmd/events.go:309-324`).
fn event_colour(event_type: &str) -> &'static str {
    if event_type.starts_with("mission.") {
        "\x1b[1;36m"
    } else if event_type.starts_with("phase.") {
        "\x1b[1;34m"
    } else if event_type.starts_with("worker.") {
        "\x1b[1;32m"
    } else if event_type.starts_with("system.") {
        "\x1b[1;33m"
    } else if event_type.starts_with("dag.") {
        "\x1b[35m"
    } else {
        "\x1b[0m"
    }
}

/// Matches Go's `fmt.Sprintf("%v", v)` for a JSON-decoded `interface{}`
/// value (`internal/cmd/events.go:297`): strings print unquoted,
/// booleans/null use Go's literal spelling, float64-backed numbers use Go's
/// shortest `%g` form, arrays use `[a b]`, and sorted-key objects use
/// `map[k:v ...]`. The explicit heap-backed work stack keeps rendering safe
/// at Go's 10,000-container JSON nesting limit.
fn go_display_value(value: &serde_json::Value) -> String {
    enum RenderFrame<'a> {
        Value(&'a serde_json::Value),
        Text(&'a str),
    }

    let mut output = String::new();
    let mut stack = vec![RenderFrame::Value(value)];
    while let Some(frame) = stack.pop() {
        match frame {
            RenderFrame::Text(text) => output.push_str(text),
            RenderFrame::Value(value) => match value {
                serde_json::Value::Null => output.push_str("<nil>"),
                serde_json::Value::Bool(flag) => {
                    output.push_str(if *flag { "true" } else { "false" });
                }
                serde_json::Value::String(text) => output.push_str(text),
                serde_json::Value::Number(number) => {
                    output.push_str(&go_display_number(number));
                }
                serde_json::Value::Array(items) => {
                    output.push('[');
                    stack.push(RenderFrame::Text("]"));
                    for (index, item) in items.iter().enumerate().rev() {
                        stack.push(RenderFrame::Value(item));
                        if index > 0 {
                            stack.push(RenderFrame::Text(" "));
                        }
                    }
                }
                serde_json::Value::Object(map) => {
                    output.push_str("map[");
                    stack.push(RenderFrame::Text("]"));
                    for (index, (key, value)) in map.iter().enumerate().rev() {
                        stack.push(RenderFrame::Value(value));
                        stack.push(RenderFrame::Text(":"));
                        stack.push(RenderFrame::Text(key));
                        if index > 0 {
                            stack.push(RenderFrame::Text(" "));
                        }
                    }
                }
            },
        }
    }
    output
}

fn go_display_number(number: &serde_json::Number) -> String {
    if !number.is_f64() {
        return number.to_string();
    }
    let Some(value) = number.as_f64() else {
        return number.to_string();
    };
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0".to_owned()
        } else {
            "0".to_owned()
        };
    }

    // Serde supplies shortest round-tripping digits; Go `%g` uses the same
    // digits but chooses exponent notation at different decimal thresholds.
    let shortest = number.to_string();
    let unsigned = shortest.strip_prefix('-').unwrap_or(&shortest);
    let (mantissa, explicit_exponent) = unsigned
        .split_once(['e', 'E'])
        .map_or((unsigned, 0), |(mantissa, exponent)| {
            (mantissa, exponent.parse::<i32>().unwrap_or_default())
        });
    let decimal_point = mantissa.find('.').unwrap_or(mantissa.len());
    let digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).collect();
    let Some(first_nonzero) = digits.iter().position(|digit| *digit != b'0') else {
        return if value.is_sign_negative() {
            "-0".to_owned()
        } else {
            "0".to_owned()
        };
    };
    let last_nonzero = digits
        .iter()
        .rposition(|digit| *digit != b'0')
        .unwrap_or(first_nonzero);
    let significant = &digits[first_nonzero..=last_nonzero];
    let exponent = explicit_exponent + decimal_point as i32 - first_nonzero as i32 - 1;

    let mut output = String::new();
    if value.is_sign_negative() {
        output.push('-');
    }
    if !(-4..6).contains(&exponent) {
        output.push(char::from(significant[0]));
        if significant.len() > 1 {
            output.push('.');
            for digit in &significant[1..] {
                output.push(char::from(*digit));
            }
        }
        output.push('e');
        if exponent < 0 {
            output.push('-');
        } else {
            output.push('+');
        }
        let magnitude = exponent.unsigned_abs().to_string();
        if magnitude.len() < 2 {
            output.push('0');
        }
        output.push_str(&magnitude);
        return output;
    }

    let fixed_point = exponent + 1;
    if fixed_point <= 0 {
        output.push_str("0.");
        output.push_str(&"0".repeat(fixed_point.unsigned_abs() as usize));
        for digit in significant {
            output.push(char::from(*digit));
        }
    } else {
        let fixed_point = fixed_point as usize;
        for digit in &significant[..significant.len().min(fixed_point)] {
            output.push(char::from(*digit));
        }
        if fixed_point < significant.len() {
            output.push('.');
            for digit in &significant[fixed_point..] {
                output.push(char::from(*digit));
            }
        } else {
            output.push_str(&"0".repeat(fixed_point - significant.len()));
        }
    }
    output
}

// ---- tail -------------------------------------------------------------

const TAIL_LOG_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const TAIL_LOG_WAIT_INTERVAL: Duration = Duration::from_millis(200);

/// Ports `runEventsTail` (`internal/cmd/events.go:136-204`), including clean
/// SIGINT/SIGTERM termination.
pub(crate) fn run_tail(
    home: &Path,
    mission_id_argument: &str,
    raw_json: bool,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), EventsError> {
    let signals = TailSignals::new()?;

    // When the B1 daemon is present, use its blocking, bounded broadcast
    // socket. This removes periodic file polling from the composed service and
    // supplies a durable sequence cursor for reconnect/restart. Resolve the
    // selected log first, then establish the daemon subscription before any
    // file-end boundary is sampled. The daemon's owner-locked high-water/live
    // handoff is therefore the only attach boundary; there is no file-to-stream
    // interval in which an event can disappear.
    if let Ok(client) = DaemonClient::open(home) {
        if matches!(client.identity(), Ok(Some(_))) {
            let resolved = resolve_log_target(home, mission_id_argument, LogUse::Tail)?;
            if resolved.daemon_stream_eligible {
                let mission_filter = resolved
                    .path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or(mission_id_argument);
                let mut cursor = -1;
                let initial = client.subscribe_with_cursor(&mut cursor).ok();
                writeln!(
                    error_output,
                    "tailing {} (Ctrl-C to stop)...",
                    resolved
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or(""),
                )?;
                return run_daemon_tail(
                    home,
                    DaemonTailConnection {
                        client,
                        cursor,
                        initial,
                    },
                    mission_filter,
                    raw_json,
                    &signals,
                    output,
                );
            }
        }
    }

    let (path, mut file, mut offset) = open_tail_log(home, mission_id_argument)?;
    writeln!(
        error_output,
        "tailing {} (Ctrl-C to stop)...",
        path.file_name().and_then(|n| n.to_str()).unwrap_or(""),
    )?;

    const POLL_INTERVAL: Duration = Duration::from_millis(100);
    loop {
        if signals.stopped() {
            return Ok(());
        }
        let read = tail_reader(&mut file, offset).map_err(event_log_read_error)?;
        let next_offset = render_tail_read(read, raw_json, output)?;
        let idle = next_offset == offset;
        file.seek(SeekFrom::Start(next_offset))
            .map_err(EventsError::ReadLog)?;
        offset = next_offset;
        if idle {
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

struct DaemonTailConnection {
    client: DaemonClient,
    cursor: i64,
    initial: Option<DaemonSubscription>,
}

fn run_daemon_tail(
    home: &Path,
    connection: DaemonTailConnection,
    mission_id: &str,
    raw_json: bool,
    signals: &TailSignals,
    output: &mut impl Write,
) -> Result<(), EventsError> {
    let DaemonTailConnection {
        mut client,
        mut cursor,
        mut initial,
    } = connection;
    let mut reconnect_deadline = None;
    loop {
        if signals.stopped() {
            return Ok(());
        }
        let mut subscription = match initial
            .take()
            .map_or_else(|| client.subscribe_with_cursor(&mut cursor), Ok)
        {
            Ok(subscription) => {
                reconnect_deadline = None;
                subscription
            }
            Err(_) => {
                let deadline = reconnect_deadline.get_or_insert_with(|| {
                    Instant::now()
                        .checked_add(TAIL_LOG_WAIT_TIMEOUT)
                        .unwrap_or_else(Instant::now)
                });
                let now = Instant::now();
                if now >= *deadline {
                    return Err(EventsError::ReadLog(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "daemon event stream did not recover within 10s",
                    )));
                }
                signals.wait_or_stopped(
                    deadline
                        .saturating_duration_since(now)
                        .min(Duration::from_millis(50)),
                );
                if let Ok(reopened) = DaemonClient::open(home) {
                    client = reopened;
                }
                continue;
            }
        };
        signals.install(
            subscription
                .cancellation_handle()
                .map_err(|error| EventsError::ReadLog(std::io::Error::other(error)))?,
        );
        loop {
            if signals.stopped() {
                return Ok(());
            }
            let frame = match subscription.read_event() {
                Ok(None) => break,
                Err(DaemonError::Io { source, .. })
                    if signals.stopped()
                        && matches!(
                            source.kind(),
                            std::io::ErrorKind::BrokenPipe
                                | std::io::ErrorKind::ConnectionAborted
                                | std::io::ErrorKind::NotConnected
                        ) =>
                {
                    return Ok(());
                }
                Err(_) => break,
                Ok(Some(frame)) => frame,
            };
            let event = decode_event_line(&frame.ingress_bytes).map_err(|error| {
                EventsError::ReadLog(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
            })?;
            cursor = cursor.max(frame.cursor);
            if event.record.mission_id != mission_id {
                continue;
            }
            if raw_json {
                write_tail_raw_line(output, &frame.ingress_bytes)?;
            } else {
                format_event_line(&event.record, output)?;
            }
            output.flush()?;
        }
        signals.clear();
    }
}

struct TailSignals {
    stopped: Arc<AtomicBool>,
    current: Arc<Mutex<Option<DaemonSubscriptionCancel>>>,
    wake: Arc<(Mutex<()>, Condvar)>,
    handle: SignalHandle,
    thread: Option<JoinHandle<()>>,
}

impl TailSignals {
    fn new() -> Result<Self, EventsError> {
        let mut signals = Signals::new([SIGINT, SIGTERM]).map_err(EventsError::ReadLog)?;
        let handle = signals.handle();
        let stopped = Arc::new(AtomicBool::new(false));
        let current = Arc::new(Mutex::new(None::<DaemonSubscriptionCancel>));
        let wake = Arc::new((Mutex::new(()), Condvar::new()));
        let signal_stopped = Arc::clone(&stopped);
        let signal_current = Arc::clone(&current);
        let signal_wake = Arc::clone(&wake);
        let thread = match thread::Builder::new()
            .name("orchestrator-events-tail-signals".into())
            .spawn(move || {
                if signals.forever().next().is_some() {
                    signal_stopped.store(true, Ordering::Release);
                    if let Some(cancel) = lock_unpoisoned(&signal_current).as_ref() {
                        cancel.cancel();
                    }
                    signal_wake.1.notify_all();
                }
            }) {
            Ok(thread) => thread,
            Err(source) => {
                handle.close();
                return Err(EventsError::ReadLog(source));
            }
        };
        Ok(Self {
            stopped,
            current,
            wake,
            handle,
            thread: Some(thread),
        })
    }

    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    fn install(&self, cancel: DaemonSubscriptionCancel) {
        let mut current = lock_unpoisoned(&self.current);
        if self.stopped() {
            cancel.cancel();
        } else {
            *current = Some(cancel);
        }
    }

    fn clear(&self) {
        lock_unpoisoned(&self.current).take();
    }

    fn wait_or_stopped(&self, duration: Duration) {
        if self.stopped() {
            return;
        }
        let guard = lock_unpoisoned(&self.wake.0);
        let _guard = self
            .wake
            .1
            .wait_timeout(guard, duration)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

impl Drop for TailSignals {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(cancel) = lock_unpoisoned(&self.current).take() {
            cancel.cancel();
        }
        self.wake.1.notify_all();
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn render_tail_read(
    read: TailRead,
    raw_json: bool,
    output: &mut impl Write,
) -> Result<u64, EventsError> {
    let next_offset = read.next_offset;
    for record in read {
        match record.map_err(event_log_read_error)? {
            TailRecord::Event { event, .. } => {
                if raw_json {
                    write_tail_raw_line(output, &event.raw_line)?;
                } else {
                    format_event_line(&event.record, output)?;
                }
            }
            TailRecord::Corrupt { raw_line, .. } => write_tail_raw_line(output, &raw_line)?,
        }
    }
    Ok(next_offset)
}

fn open_tail_log(
    home: &Path,
    mission_id_argument: &str,
) -> Result<(PathBuf, std::fs::File, u64), EventsError> {
    open_tail_log_with_wait(home, mission_id_argument, |resolved| {
        wait_for_tail_file_with(
            &resolved.path,
            TAIL_LOG_WAIT_TIMEOUT,
            || resolved.target.open(),
            Instant::now,
            std::thread::sleep,
        )
    })
}

fn open_tail_log_with_wait<Wait>(
    home: &Path,
    mission_id_argument: &str,
    wait_for_file: Wait,
) -> Result<(PathBuf, std::fs::File, u64), EventsError>
where
    Wait: FnOnce(&ResolvedLog) -> Result<std::fs::File, EventsError>,
{
    let resolved = resolve_log_target(home, mission_id_argument, LogUse::Tail)?;
    let mut file = wait_for_file(&resolved)?;
    let offset = seek_tail_to_end(&mut file, &resolved.path)?;
    Ok((resolved.path, file, offset))
}

fn wait_for_tail_file_with<Open, Now, Sleep>(
    path: &Path,
    timeout: Duration,
    mut open: Open,
    mut now: Now,
    mut sleep: Sleep,
) -> Result<std::fs::File, EventsError>
where
    Open: FnMut() -> std::io::Result<std::fs::File>,
    Now: FnMut() -> Instant,
    Sleep: FnMut(Duration),
{
    let Some(deadline) = now().checked_add(timeout) else {
        return Err(tail_log_wait_timeout(path, timeout));
    };

    loop {
        match open() {
            Ok(file) => return Ok(file),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(open_tail_log_error(path, source)),
        }

        // Go uses time.Now().After(deadline), so equality gets one more poll.
        if now() > deadline {
            return Err(tail_log_wait_timeout(path, timeout));
        }
        sleep(TAIL_LOG_WAIT_INTERVAL);
    }
}

fn tail_log_wait_timeout(path: &Path, timeout: Duration) -> EventsError {
    EventsError::TailLogWaitTimeout {
        path: path.display().to_string(),
        timeout_seconds: timeout.as_secs(),
    }
}

fn seek_tail_to_end(file: &mut impl Seek, path: &Path) -> Result<u64, EventsError> {
    file.seek(SeekFrom::End(0))
        .map_err(|source| seek_log_error(path, source))
}

fn open_log_error(path: &Path, source: std::io::Error) -> EventsError {
    EventsError::OpenLog {
        path: path.display().to_string(),
        message: go_io_error_message(&source),
        source,
    }
}

fn open_tail_log_error(path: &Path, source: std::io::Error) -> EventsError {
    EventsError::OpenTailLog {
        path: path.display().to_string(),
        message: go_io_error_message(&source),
        source,
    }
}

fn seek_log_error(path: &Path, source: std::io::Error) -> EventsError {
    EventsError::SeekLog {
        path: path.display().to_string(),
        message: go_io_error_message(&source),
        source,
    }
}

fn event_log_read_error(source: EventLogIoError) -> EventsError {
    match source {
        EventLogIoError::Io { source, .. } => EventsError::ReadLog(source),
        EventLogIoError::OversizedLine => EventsError::ReadLog(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "event log line exceeds the 64 KiB scanner buffer",
        )),
        source => EventsError::ReadLog(std::io::Error::other(source)),
    }
}

fn go_io_error_message(source: &std::io::Error) -> String {
    let rendered = source.to_string();
    let Some(raw_code) = source.raw_os_error() else {
        return rendered;
    };
    let rust_suffix = format!(" (os error {raw_code})");
    rendered
        .strip_suffix(&rust_suffix)
        .unwrap_or(&rendered)
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    const NUMERIC_EVENT_LINE: &str = concat!(
        "{\"id\":\"evt_numbers\",\"type\":\"mission.started\",",
        "\"timestamp\":\"2026-07-13T00:00:00Z\",\"sequence\":1,",
        "\"mission_id\":\"m1\",\"data\":{",
        "\"integral\":3,\"fractional\":3.5,\"positive_exponent\":1234567.8,",
        "\"negative_exponent\":0.00004,\"negative_zero\":-0}}",
    );
    const NUMERIC_CONTEXT: &str = concat!(
        "  fractional=3.5 integral=3 negative_exponent=4e-05",
        " negative_zero=-0 positive_exponent=1.2345678e+06\n",
    );

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("events-cmd-test-{label}-{}", std::process::id()))
    }

    fn nested_formatted_event_line(array_depth: usize) -> Vec<u8> {
        const PREFIX: &[u8] = concat!(
            "{\"id\":\"evt_deep\",\"type\":\"mission.started\",",
            "\"timestamp\":\"2026-07-13T00:00:00Z\",\"sequence\":1,",
            "\"mission_id\":\"m1\",\"data\":{\"nested\":",
        )
        .as_bytes();

        let mut line = Vec::with_capacity(PREFIX.len() + array_depth * 2 + 5);
        line.extend_from_slice(PREFIX);
        line.extend(std::iter::repeat_n(b'[', array_depth));
        line.push(b'0');
        line.extend(std::iter::repeat_n(b']', array_depth));
        line.extend_from_slice(b"}}\r\n");
        line
    }

    #[test]
    fn list_reports_missing_directory() -> TestResult {
        let home = temp_dir("missing-dir");
        let mut output = Vec::new();
        run_list(&home, &mut output)?;
        assert_eq!(
            String::from_utf8(output)?,
            "no event logs found (directory does not exist yet)\n"
        );
        Ok(())
    }

    #[test]
    fn list_reports_counts_and_newest_first() -> TestResult {
        let home = temp_dir("counts");
        std::fs::create_dir_all(home.join("events"))?;
        std::fs::write(
            home.join("events").join("20260101-aaa.jsonl"),
            "line1\nline2\n",
        )?;
        std::fs::write(
            home.join("events").join("20260102-bbb.jsonl"),
            "line1\n\nline3\n",
        )?;
        let mut output = Vec::new();
        run_list(&home, &mut output)?;
        std::fs::remove_dir_all(&home)?;

        let text = String::from_utf8(output)?;
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[2].starts_with("20260102-bbb"), "{text}");
        assert!(lines[3].starts_with("20260101-aaa"), "{text}");
        Ok(())
    }

    #[test]
    fn replay_prints_formatted_events_and_raw_fallback_for_corrupt_lines() -> TestResult {
        let home = temp_dir("replay");
        std::fs::create_dir_all(home.join("events"))?;
        std::fs::write(
            home.join("events").join("m1.jsonl"),
            "{\"id\":\"evt_1\",\"type\":\"mission.started\",\"timestamp\":\"2026-07-13T00:00:00Z\",\"sequence\":1,\"mission_id\":\"m1\"}\n\
             not json\n",
        )?;
        let mut output = Vec::new();
        run_replay(&home, "m1", false, &mut output)?;
        std::fs::remove_dir_all(&home)?;

        let text = String::from_utf8(output)?;
        assert!(text.contains("mission.started"), "{text}");
        assert!(text.contains("not json"), "{text}");
        Ok(())
    }

    #[test]
    fn replay_raw_json_passes_lines_through_verbatim() -> TestResult {
        let home = temp_dir("replay-raw");
        std::fs::create_dir_all(home.join("events"))?;
        let line = "{\"id\":\"evt_1\",\"type\":\"mission.started\",\"timestamp\":\"2026-07-13T00:00:00Z\",\"sequence\":1,\"mission_id\":\"m1\"}";
        std::fs::write(home.join("events").join("m1.jsonl"), format!("{line}\n"))?;
        let mut output = Vec::new();
        run_replay(&home, "m1", true, &mut output)?;
        std::fs::remove_dir_all(&home)?;
        assert_eq!(String::from_utf8(output)?, format!("{line}\n"));
        Ok(())
    }

    #[test]
    fn replay_formats_decoded_data_numbers_like_go_float64_percent_v() -> TestResult {
        let home = temp_dir("replay-numeric-data");
        std::fs::create_dir_all(home.join("events"))?;
        std::fs::write(
            home.join("events/m1.jsonl"),
            format!("{NUMERIC_EVENT_LINE}\n"),
        )?;

        let mut output = Vec::new();
        run_replay(&home, "m1", false, &mut output)?;
        std::fs::remove_dir_all(&home)?;

        let text = String::from_utf8(output)?;
        assert!(
            text.ends_with(NUMERIC_CONTEXT),
            "unexpected replay output: {text:?}"
        );
        Ok(())
    }

    #[test]
    fn replay_formats_and_drops_data_at_go_maximum_nesting_without_stack_overflow() -> TestResult {
        // The event root and data object consume two of Go's 10,000 containers.
        let array_depth = 9_998;
        let line = nested_formatted_event_line(array_depth);
        assert!(line.len() < SCANNER_MAX_TOKEN_BYTES);

        let home = temp_dir("replay-deep-formatted");
        std::fs::create_dir_all(home.join("events"))?;
        std::fs::write(home.join("events/m1.jsonl"), &line)?;

        let mut output = Vec::new();
        run_replay(&home, "m1", false, &mut output)?;
        std::fs::remove_dir_all(&home)?;

        let mut expected_suffix = Vec::with_capacity(array_depth * 2 + 12);
        expected_suffix.extend_from_slice(b"  nested=");
        expected_suffix.extend(std::iter::repeat_n(b'[', array_depth));
        expected_suffix.push(b'0');
        expected_suffix.extend(std::iter::repeat_n(b']', array_depth));
        expected_suffix.push(b'\n');
        assert!(output.ends_with(&expected_suffix));
        Ok(())
    }

    #[test]
    fn replay_scanner_drops_final_carriage_return_from_valid_eof_token() -> TestResult {
        let home = temp_dir("replay-valid-final-cr");
        std::fs::create_dir_all(home.join("events"))?;
        std::fs::write(
            home.join("events/m1.jsonl"),
            format!("{NUMERIC_EVENT_LINE}\r"),
        )?;

        let mut output = Vec::new();
        run_replay(&home, "m1", true, &mut output)?;
        std::fs::remove_dir_all(&home)?;

        assert_eq!(
            String::from_utf8(output)?,
            format!("{NUMERIC_EVENT_LINE}\n")
        );
        Ok(())
    }

    #[test]
    fn replay_scanner_drops_final_carriage_return_from_corrupt_eof_token() -> TestResult {
        let home = temp_dir("replay-corrupt-final-cr");
        std::fs::create_dir_all(home.join("events"))?;
        std::fs::write(home.join("events/m1.jsonl"), b"not json\r")?;

        let mut output = Vec::new();
        run_replay(&home, "m1", false, &mut output)?;
        std::fs::remove_dir_all(&home)?;

        assert_eq!(output, b"not json\n");
        Ok(())
    }

    #[test]
    fn replay_stream_preserves_event_corrupt_event_order() -> TestResult {
        let home = temp_dir("replay-ordered");
        std::fs::create_dir_all(home.join("events"))?;
        let first = "{\"id\":\"evt_1\",\"type\":\"mission.started\",\"timestamp\":\"2026-07-13T00:00:00Z\",\"sequence\":1,\"mission_id\":\"m1\"}";
        let last = "{\"id\":\"evt_2\",\"type\":\"mission.completed\",\"timestamp\":\"2026-07-13T00:00:01Z\",\"sequence\":2,\"mission_id\":\"m1\"}";
        let input = format!("{first}\nnot json\n{last}\n");
        std::fs::write(home.join("events/m1.jsonl"), &input)?;

        let mut output = Vec::new();
        run_replay(&home, "m1", true, &mut output)?;
        std::fs::remove_dir_all(&home)?;

        assert_eq!(String::from_utf8(output)?, input);
        Ok(())
    }

    #[test]
    fn replay_missing_mission_errors() {
        let home = temp_dir("replay-missing");
        let mut output = Vec::new();
        let result = run_replay(&home, "nonexistent", false, &mut output);
        assert!(matches!(result, Err(EventsError::NotFound { .. })));
    }

    #[test]
    fn resolve_log_path_accepts_direct_jsonl_path() -> TestResult {
        let home = temp_dir("direct-path");
        std::fs::create_dir_all(&home)?;
        let direct = home.join("standalone.jsonl");
        std::fs::write(&direct, "")?;
        let resolved = resolve_log_target(
            &home,
            direct.to_str().ok_or("non-utf8 path")?,
            LogUse::Replay,
        )?;
        std::fs::remove_dir_all(&home)?;
        assert_eq!(resolved.path, direct);
        Ok(())
    }

    #[test]
    fn direct_jsonl_resolution_preserves_unclean_display_spelling() -> TestResult {
        let home = temp_dir("direct-unclean-spelling");
        let traversed = home.join("traversed");
        std::fs::create_dir_all(&traversed)?;
        std::fs::write(home.join("standalone.jsonl"), b"event\n")?;
        let direct = traversed.join("../standalone.jsonl");
        let argument = direct.to_str().ok_or("non-utf8 path")?;

        let resolved = resolve_log_target(&home, argument, LogUse::Replay)?;
        assert_eq!(resolved.path, PathBuf::from(argument));
        assert_eq!(resolved.path.display().to_string(), argument);
        drop(resolved);

        std::fs::remove_dir_all(&home)?;
        Ok(())
    }

    #[test]
    fn fallback_clean_matches_go_for_parent_duplicate_and_dot_components() -> TestResult {
        assert_eq!(
            clean_fallback_relative("/tmp/../foo.jsonl")?,
            PathBuf::from("foo.jsonl.jsonl")
        );
        assert_eq!(
            clean_fallback_relative("/tmp//./foo.jsonl")?,
            PathBuf::from("tmp/foo.jsonl.jsonl")
        );
        assert!(matches!(
            clean_fallback_relative("/../../foo.jsonl"),
            Err(EventsError::UnsafePath { mission_id })
                if mission_id == "/../../foo.jsonl"
        ));
        Ok(())
    }

    #[test]
    fn missing_absolute_jsonl_falls_back_under_home_without_opening_outside() -> TestResult {
        let root = temp_dir("missing-absolute-oracle");
        let home = root.join("home");
        std::fs::create_dir_all(home.join("events"))?;

        let direct = root.join("nanika-absent.jsonl");
        let outside_fallback = PathBuf::from(format!("{}.jsonl", direct.display()));
        std::fs::write(
            &outside_fallback,
            b"{\"id\":\"outside\",\"type\":\"mission.started\",\"timestamp\":\"2026-07-17T00:00:00Z\",\"sequence\":1,\"mission_id\":\"outside\"}\n",
        )?;

        let argument = direct.to_str().ok_or("non-utf8 path")?.to_owned();
        let relative = direct.strip_prefix(Path::new("/"))?;
        let expected_path = home
            .join("events")
            .join(format!("{}.jsonl", relative.display()))
            .display()
            .to_string();
        let mut output = Vec::new();
        let result = run_replay(&home, &argument, false, &mut output);
        std::fs::remove_dir_all(&root)?;

        assert!(output.is_empty());
        assert!(matches!(
            result,
            Err(EventsError::NotFound { mission_id, path })
                if mission_id == argument && path == expected_path
        ));
        Ok(())
    }

    #[test]
    fn tail_opens_an_existing_direct_jsonl_path_at_end() -> TestResult {
        let root = temp_dir("tail-direct-path");
        let home = root.join("home");
        let direct = root.join("standalone.jsonl");
        std::fs::create_dir_all(&home)?;
        std::fs::write(&direct, b"existing event\n")?;

        let (resolved, file, offset) =
            open_tail_log(&home, direct.to_str().ok_or("non-utf8 path")?)?;
        drop(file);
        std::fs::remove_dir_all(&root)?;

        assert_eq!(resolved, direct);
        assert_eq!(offset, 15);
        Ok(())
    }

    #[test]
    fn tail_retries_when_resolved_log_disappears_before_open() -> TestResult {
        use std::cell::Cell;

        let root = temp_dir("tail-resolve-open-race");
        let home = root.join("home");
        let direct = root.join("standalone.jsonl");
        std::fs::create_dir_all(&home)?;
        std::fs::write(&direct, b"initial event\n")?;

        let open_attempts = Cell::new(0u8);
        let sleep_calls = Cell::new(0u8);
        let start = Instant::now();
        let argument = direct.to_str().ok_or("non-utf8 path")?;
        let (resolved, file, offset) = open_tail_log_with_wait(&home, argument, |resolved_log| {
            std::fs::remove_file(&resolved_log.path)
                .map_err(|source| open_tail_log_error(&resolved_log.path, source))?;
            wait_for_tail_file_with(
                &resolved_log.path,
                Duration::from_secs(1),
                || {
                    let attempt = open_attempts.get().saturating_add(1);
                    open_attempts.set(attempt);
                    if attempt == 2 {
                        std::fs::write(&resolved_log.path, b"reappeared\n")?;
                    }
                    resolved_log.target.open()
                },
                || start,
                |_| sleep_calls.set(sleep_calls.get().saturating_add(1)),
            )
        })?;
        drop(file);
        std::fs::remove_dir_all(&root)?;

        assert_eq!(resolved, direct);
        assert_eq!(offset, u64::try_from(b"reappeared\n".len())?);
        assert_eq!(open_attempts.get(), 2);
        assert_eq!(sleep_calls.get(), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn tail_rejects_final_symlink_swap_after_resolution() -> TestResult {
        use std::cell::Cell;

        let root = temp_dir("tail-final-symlink-swap");
        let home = root.join("home");
        let direct = root.join("standalone.jsonl");
        let outside = root.join("outside.jsonl");
        std::fs::create_dir_all(&home)?;
        std::fs::write(&direct, b"inside\n")?;
        std::fs::write(&outside, b"outside\n")?;

        let sleep_calls = Cell::new(0u8);
        let start = Instant::now();
        let argument = direct.to_str().ok_or("non-utf8 path")?;
        let result = open_tail_log_with_wait(&home, argument, |resolved_log| {
            std::fs::remove_file(&resolved_log.path)
                .map_err(|source| open_tail_log_error(&resolved_log.path, source))?;
            std::os::unix::fs::symlink(&outside, &resolved_log.path)
                .map_err(|source| open_tail_log_error(&resolved_log.path, source))?;
            wait_for_tail_file_with(
                &resolved_log.path,
                Duration::from_secs(1),
                || resolved_log.target.open(),
                || start,
                |_| sleep_calls.set(sleep_calls.get().saturating_add(1)),
            )
        });
        std::fs::remove_dir_all(&root)?;

        assert!(matches!(
            result,
            Err(EventsError::OpenTailLog {
                path,
                source,
                ..
            }) if path == direct.display().to_string()
                && source.kind() != std::io::ErrorKind::NotFound
        ));
        assert_eq!(sleep_calls.get(), 0);
        Ok(())
    }

    #[test]
    fn tail_rejects_a_missing_canonical_log_before_polling() {
        let home = temp_dir("tail-missing-canonical");
        let result = open_tail_log(&home, "missing");
        assert!(matches!(
            result,
            Err(EventsError::NotFound { mission_id, path })
                if mission_id == "missing"
                    && path == home.join("events/missing.jsonl").display().to_string()
        ));
    }

    #[test]
    fn tail_chunk_rendering_preserves_event_corrupt_event_order() -> TestResult {
        let first = "{\"id\":\"evt_1\",\"type\":\"mission.started\",\"timestamp\":\"2026-07-13T00:00:00Z\",\"sequence\":1,\"mission_id\":\"m1\"}";
        let last = "{\"id\":\"evt_2\",\"type\":\"mission.completed\",\"timestamp\":\"2026-07-13T00:00:01Z\",\"sequence\":2,\"mission_id\":\"m1\"}";
        let input = format!("{first}\nnot json\n{last}\n");
        let base_offset = 41;
        let read = orchestrator_exec::tail_chunk(input.as_bytes(), base_offset);
        let mut output = Vec::new();

        let next_offset = render_tail_read(read, true, &mut output)?;

        assert_eq!(String::from_utf8(output)?, input);
        assert_eq!(next_offset, base_offset + u64::try_from(input.len())?);
        Ok(())
    }

    #[test]
    fn tail_formats_decoded_data_numbers_like_go_float64_percent_v() -> TestResult {
        let input = format!("{NUMERIC_EVENT_LINE}\n");
        let read = tail_reader(std::io::Cursor::new(input.as_bytes()), 0)?;
        let mut output = Vec::new();

        render_tail_read(read, false, &mut output)?;

        let text = String::from_utf8(output)?;
        assert!(
            text.ends_with(NUMERIC_CONTEXT),
            "unexpected tail output: {text:?}"
        );
        Ok(())
    }

    #[test]
    fn tail_read_string_preserves_carriage_returns_for_valid_and_corrupt_lines() -> TestResult {
        let input = format!("{NUMERIC_EVENT_LINE}\r\nnot json\r\n");
        let read = tail_reader(std::io::Cursor::new(input.as_bytes()), 0)?;
        let mut output = Vec::new();

        render_tail_read(read, true, &mut output)?;

        assert_eq!(String::from_utf8(output)?, input);
        Ok(())
    }

    #[test]
    fn tail_rendering_emits_records_before_deferred_error_without_returning_offset() -> TestResult {
        let line = "{\"id\":\"evt_1\",\"type\":\"mission.started\",\"timestamp\":\"2026-07-13T00:00:00Z\",\"sequence\":1,\"mission_id\":\"m1\"}\n";
        let mut read = orchestrator_exec::tail_chunk(line.as_bytes(), 0);
        read.deferred_error = Some(EventLogIoError::Io {
            operation: "injected tail read",
            source: std::io::Error::other("injected deferred failure"),
        });
        let mut output = Vec::new();

        let result = render_tail_read(read, true, &mut output);

        assert!(matches!(result, Err(EventsError::ReadLog(_))));
        assert_eq!(String::from_utf8(output)?, line);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn tail_open_error_keeps_path_operation_and_normalized_enotdir() -> TestResult {
        let root = temp_dir("tail-enotdir");
        let home = root.join("home");
        std::fs::create_dir_all(&home)?;
        std::fs::write(home.join("events"), b"not a directory")?;

        let result = open_tail_log(&home, "mission");
        std::fs::remove_dir_all(&root)?;

        assert!(matches!(&result, Err(EventsError::OpenTailLog { .. })));
        if let Err(error) = result {
            assert_eq!(
                error.to_string(),
                format!(
                    "opening log file: open {}: not a directory",
                    home.join("events/mission.jsonl").display()
                )
            );
        }
        Ok(())
    }

    #[test]
    fn tail_wait_timeout_is_injectable_without_wall_clock_delay() {
        use std::cell::Cell;

        let path = PathBuf::from("/tmp/nanika-tail-timeout.jsonl");
        let start = Instant::now();
        let clock_reads = Cell::new(0u8);
        let sleep_calls = Cell::new(0u8);
        let result = wait_for_tail_file_with(
            &path,
            Duration::from_secs(1),
            || {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "injected disappearance",
                ))
            },
            || {
                let read = clock_reads.get();
                clock_reads.set(read.saturating_add(1));
                if read == 0 {
                    start
                } else {
                    start + Duration::from_secs(2)
                }
            },
            |_| sleep_calls.set(sleep_calls.get().saturating_add(1)),
        );

        assert!(matches!(
            result,
            Err(EventsError::TailLogWaitTimeout {
                path: error_path,
                timeout_seconds: 1,
            }) if error_path == path.display().to_string()
        ));
        assert_eq!(clock_reads.get(), 2);
        assert_eq!(sleep_calls.get(), 0);
    }

    #[test]
    fn tail_wait_polls_once_more_at_exact_deadline() -> TestResult {
        use std::cell::Cell;

        let root = temp_dir("tail-deadline-equality");
        std::fs::create_dir_all(&root)?;
        let path = root.join("appeared.jsonl");
        std::fs::write(&path, b"appeared\n")?;
        let start = Instant::now();
        let open_attempts = Cell::new(0u8);
        let clock_reads = Cell::new(0u8);
        let sleep_calls = Cell::new(0u8);

        let file = wait_for_tail_file_with(
            &path,
            Duration::from_secs(1),
            || {
                let attempt = open_attempts.get().saturating_add(1);
                open_attempts.set(attempt);
                if attempt == 1 {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "injected disappearance",
                    ))
                } else {
                    std::fs::File::open(&path)
                }
            },
            || {
                let read = clock_reads.get();
                clock_reads.set(read.saturating_add(1));
                if read == 0 {
                    start
                } else {
                    start + Duration::from_secs(1)
                }
            },
            |_| sleep_calls.set(sleep_calls.get().saturating_add(1)),
        )?;
        drop(file);
        std::fs::remove_dir_all(&root)?;

        assert_eq!(open_attempts.get(), 2);
        assert_eq!(clock_reads.get(), 2);
        assert_eq!(sleep_calls.get(), 1);
        Ok(())
    }

    #[test]
    fn tail_wait_surfaces_non_not_found_open_error_immediately_with_go_text() {
        use std::cell::Cell;

        let path = PathBuf::from("/tmp/nanika-tail-permission.jsonl");
        let start = Instant::now();
        let clock_reads = Cell::new(0u8);
        let sleep_calls = Cell::new(0u8);
        let result = wait_for_tail_file_with(
            &path,
            Duration::from_secs(10),
            || Err(std::io::Error::from_raw_os_error(13)),
            || {
                clock_reads.set(clock_reads.get().saturating_add(1));
                start
            },
            |_| sleep_calls.set(sleep_calls.get().saturating_add(1)),
        );

        assert!(matches!(&result, Err(EventsError::OpenTailLog { .. })));
        if let Err(error) = result {
            assert_eq!(
                error.to_string(),
                "opening log file: open /tmp/nanika-tail-permission.jsonl: permission denied"
            );
        }
        assert_eq!(clock_reads.get(), 1);
        assert_eq!(sleep_calls.get(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn go_io_error_message_normalizes_representative_path_errors() {
        for (raw_code, expected) in [
            (1, "operation not permitted"),
            (2, "no such file or directory"),
            (13, "permission denied"),
            (20, "not a directory"),
            (29, "illegal seek"),
        ] {
            assert_eq!(
                go_io_error_message(&std::io::Error::from_raw_os_error(raw_code)),
                expected
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn tail_seek_error_retains_fifo_path_and_go_operation_text() -> TestResult {
        use std::{os::fd::OwnedFd, os::unix::net::UnixStream};

        let (stream, peer) = UnixStream::pair()?;
        let descriptor: OwnedFd = stream.into();
        let mut non_seekable = std::fs::File::from(descriptor);
        let path = Path::new("/tmp/nanika-events.fifo");
        let result = seek_tail_to_end(&mut non_seekable, path);
        drop(peer);

        assert!(matches!(&result, Err(EventsError::SeekLog { .. })));
        if let Err(error) = result {
            assert_eq!(
                error.to_string(),
                "seeking to end of log: seek /tmp/nanika-events.fifo: illegal seek"
            );
        }
        Ok(())
    }

    #[test]
    fn human_size_matches_go_thresholds() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn go_display_value_renders_non_numeric_scalars() {
        assert_eq!(go_display_value(&serde_json::json!(true)), "true");
        assert_eq!(go_display_value(&serde_json::json!(null)), "<nil>");
        assert_eq!(go_display_value(&serde_json::json!("hi")), "hi");
    }

    #[test]
    fn go_display_value_renders_nested_collections_in_sorted_go_style() {
        let value = serde_json::json!({
            "z": [true, null, "hi"],
            "a": {"n": 1_234_567.8},
        });

        assert_eq!(
            go_display_value(&value),
            "map[a:map[n:1.2345678e+06] z:[true <nil> hi]]"
        );
    }
}
