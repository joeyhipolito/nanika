//! Terminal-oriented rendering of orchestrator events.
//!
//! Ported from the Go orchestrator's `internal/render` package
//! (`skills/orchestrator/internal/render/terminal.go`). [`TerminalRenderer`]
//! prints formatted mission progress for a subset of [`event_type`] kinds;
//! the full 30-value `event.EventType` enumeration
//! (`skills/orchestrator/internal/event/types.go:16-133`) is not ported here
//! — only the ~19 values the Go `Emit` switch (`terminal.go:95-138`)
//! actually dispatches on. This module is presentation-layer only and has no
//! dependency on `orchestrator-core`.
//!
//! Divergence flagged for the campaign lead: `printRawEvent`
//! (`terminal.go:372-393`) formats the verbose timestamp with
//! `ev.Timestamp.Local().Format("15:04:05.000")`. The Rust workspace has no
//! local-timezone dependency (see `PORTING.md` §2.9/§WORKER RULE 2), so
//! [`format_hms_millis`] renders the same field in UTC instead of the host's
//! local zone. No Go test in `terminal_test.go` asserts a specific offset
//! (`TestVerboseMode_ShowsTimestamp` only checks for a `:`), so this does not
//! change the byte-exactness of any ported assertion, but it is a real
//! behavioral divergence from the Go binary on non-UTC hosts.

use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{IsTerminal, Stderr, Write};
use std::sync::{Mutex, MutexGuard};

/// Event type strings the terminal renderer switches on.
///
/// Mirrors the subset of `event.EventType` constants
/// (`skills/orchestrator/internal/event/types.go:16-133`) that
/// `TerminalRenderer::Emit` (`terminal.go:87-139`) reads.
pub mod event_type {
    pub const DECOMPOSE_STARTED: &str = "decompose.started";
    pub const DECOMPOSE_COMPLETED: &str = "decompose.completed";
    pub const DECOMPOSE_FALLBACK: &str = "decompose.fallback";

    pub const MISSION_STARTED: &str = "mission.started";
    pub const MISSION_COMPLETED: &str = "mission.completed";
    pub const MISSION_FAILED: &str = "mission.failed";
    pub const MISSION_CANCELLED: &str = "mission.cancelled";

    pub const PHASE_STARTED: &str = "phase.started";
    pub const PHASE_COMPLETED: &str = "phase.completed";
    pub const PHASE_FAILED: &str = "phase.failed";
    pub const PHASE_SKIPPED: &str = "phase.skipped";
    pub const PHASE_RETRYING: &str = "phase.retrying";

    pub const WORKER_OUTPUT: &str = "worker.output";

    pub const GIT_WORKTREE_CREATED: &str = "git.worktree_created";
    pub const GIT_COMMITTED: &str = "git.committed";
    pub const GIT_PR_CREATED: &str = "git.pr_created";

    pub const FILE_OVERLAP_DETECTED: &str = "file_overlap.detected";

    pub const SYSTEM_ERROR: &str = "system.error";
}

/// Minimal `event.Event` envelope (`skills/orchestrator/internal/event/types.go:137-146`),
/// carrying only the fields the terminal renderer reads. `mission_id` and
/// `id` are never used by `Emit` and are intentionally not ported.
#[derive(Debug, Clone)]
pub struct Event {
    pub event_type: String,
    /// Nanoseconds since the Unix epoch. Go's `time.Time` has no direct Rust
    /// equivalent in this dependency-free workspace (see `PORTING.md` §2.9);
    /// a signed nanosecond count preserves Go's ability to compute a
    /// negative `Sub` duration without reaching for `std::time::Duration`,
    /// which is unsigned (`PORTING.md` §2.10/§3).
    pub timestamp_unix_nanos: i64,
    pub sequence: i64,
    pub phase_id: String,
    pub worker_id: String,
    pub data: BTreeMap<String, Value>,
}

/// ANSI color codes for terminal output. Empty strings when the output is
/// not a TTY. Ported from `palette` (`terminal.go:17-27`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Palette {
    pub reset: &'static str,
    pub bold: &'static str,
    pub dim: &'static str,
    pub cyan: &'static str,
    pub green: &'static str,
    pub yellow: &'static str,
    pub red: &'static str,
    pub blue: &'static str,
    pub magenta: &'static str,
}

/// Ported from `detectPalette` (`terminal.go:29-45`). Go checks
/// `os.ModeCharDevice` on the file's `Stat()`; the direct Rust analog is
/// `std::io::IsTerminal`, stable in `std` since 1.70 — no new dependency.
#[must_use]
pub fn detect_palette<T: IsTerminal>(w: &T) -> Palette {
    if w.is_terminal() {
        Palette {
            reset: "\u{1b}[0m",
            bold: "\u{1b}[1m",
            dim: "\u{1b}[2m",
            cyan: "\u{1b}[36m",
            green: "\u{1b}[32m",
            yellow: "\u{1b}[33m",
            red: "\u{1b}[31m",
            blue: "\u{1b}[34m",
            magenta: "\u{1b}[35m",
        }
    } else {
        Palette::default()
    }
}

#[derive(Clone, Debug, Default)]
struct PhaseMeta {
    name: String,
    // Ported from Go's `phaseMeta.persona` (terminal.go:70), which Go itself
    // writes in `printPhaseStarted` (terminal.go:238) but never reads.
    #[expect(
        dead_code,
        reason = "write-only in the Go source too — kept for struct-shape parity"
    )]
    persona: String,
}

struct RendererState<W: Write> {
    w: W,
    phase_starts: BTreeMap<String, i64>,
    phase_meta: BTreeMap<String, PhaseMeta>,
    completed: i64,
    failed: i64,
    skipped: i64,
}

/// Prints formatted mission progress. Ported from `TerminalRenderer`
/// (`terminal.go:53-84`). Go's `sync.Mutex` guards `phaseStarts`/`phaseMeta`
/// and the outcome counters for the whole body of `Emit`; the Rust port
/// keeps the writer under the same lock since `Emit` in Go also writes to
/// `r.w` while holding `mu`.
pub struct TerminalRenderer<W: Write> {
    c: Palette,
    verbose: bool,
    state: Mutex<RendererState<W>>,
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

impl<W: Write> TerminalRenderer<W> {
    /// Ported from `NewTerminalRenderer` (`terminal.go:75-84`), generalized
    /// over the writer and palette so tests can inject a buffer and a
    /// no-color palette exactly like `terminal_test.go`'s struct literals.
    pub fn new(writer: W, palette: Palette, verbose: bool) -> Self {
        Self {
            c: palette,
            verbose,
            state: Mutex::new(RendererState {
                w: writer,
                phase_starts: BTreeMap::new(),
                phase_meta: BTreeMap::new(),
                completed: 0,
                failed: 0,
                skipped: 0,
            }),
        }
    }

    /// Handles a single event and prints the appropriate terminal output.
    /// Ported from `Emit` (`terminal.go:87-139`); the Go `context.Context`
    /// parameter is dropped (see `PORTING.md` §2.8 — pure-logic Rust code
    /// has no `Context`-equivalent parameter).
    pub fn emit(&self, ev: &Event) {
        let mut state = lock_unpoisoned(&self.state);
        if self.verbose {
            self.print_raw_event(&mut state, ev);
        }
        match ev.event_type.as_str() {
            event_type::DECOMPOSE_STARTED => self.print_decompose_started(&mut state, ev),
            event_type::DECOMPOSE_COMPLETED => self.print_decompose_completed(&mut state, ev),
            event_type::DECOMPOSE_FALLBACK => self.print_decompose_fallback(&mut state, ev),

            event_type::MISSION_STARTED => self.print_mission_started(&mut state, ev),
            event_type::MISSION_COMPLETED => self.print_mission_completed(&mut state, ev),
            event_type::MISSION_FAILED => self.print_mission_failed(&mut state, ev),
            event_type::MISSION_CANCELLED => self.print_mission_cancelled(&mut state, ev),

            event_type::PHASE_STARTED => self.print_phase_started(&mut state, ev),
            event_type::PHASE_COMPLETED => self.print_phase_completed(&mut state, ev),
            event_type::PHASE_FAILED => self.print_phase_failed(&mut state, ev),
            event_type::PHASE_SKIPPED => self.print_phase_skipped(&mut state, ev),
            event_type::PHASE_RETRYING => self.print_phase_retrying(&mut state, ev),

            event_type::WORKER_OUTPUT => self.print_worker_output(&mut state, ev),

            event_type::GIT_WORKTREE_CREATED => self.print_git_worktree_created(&mut state, ev),
            event_type::GIT_COMMITTED => self.print_git_committed(&mut state, ev),
            event_type::GIT_PR_CREATED => self.print_git_pr_created(&mut state, ev),

            event_type::FILE_OVERLAP_DETECTED => self.print_file_overlap_detected(&mut state, ev),

            event_type::SYSTEM_ERROR => self.print_system_error(&mut state, ev),

            _ => {}
        }
    }

    /// Ported from `Close` (`terminal.go:142`) — a no-op; the renderer does
    /// not own the writer's lifecycle beyond its own drop.
    pub const fn close(&self) {}

    // --- decomposition ------------------------------------------------

    fn print_decompose_started(&self, state: &mut RendererState<W>, ev: &Event) {
        let mut summary = data_str(&ev.data, "task_summary");
        if summary.is_empty() {
            summary = "task".to_owned();
        }
        if summary.len() > 80 {
            summary = format!("{}...", truncate_at_byte_boundary(&summary, 77));
        }
        let _ = writeln!(
            state.w,
            "{}{}▸ decomposing:{} {}",
            self.c.bold, self.c.cyan, self.c.reset, summary
        );
    }

    fn print_decompose_completed(&self, state: &mut RendererState<W>, ev: &Event) {
        let count = data_int(&ev.data, "phase_count");
        let mode = data_str(&ev.data, "execution_mode");
        let _ = writeln!(
            state.w,
            "{}{}▸ plan:{} {count} phase(s), {mode}",
            self.c.bold, self.c.cyan, self.c.reset
        );

        if let Some(Value::Array(phases)) = ev.data.get("phases") {
            for (index, raw) in phases.iter().enumerate() {
                let Some(phase) = raw.as_object() else {
                    continue;
                };
                let name = str_val(phase, "name");
                let persona = str_val(phase, "persona");
                let idx = index + 1;
                let _ = writeln!(
                    state.w,
                    "  {}{idx}{}. {name} {}({persona}){}",
                    self.c.dim, self.c.reset, self.c.dim, self.c.reset
                );
            }
            let _ = writeln!(state.w);
        }
    }

    fn print_decompose_fallback(&self, state: &mut RendererState<W>, ev: &Event) {
        let reason = data_str(&ev.data, "reason");
        let _ = writeln!(
            state.w,
            "{}{}▸ decompose fallback:{} {reason}",
            self.c.bold, self.c.yellow, self.c.reset
        );
    }

    // --- mission lifecycle ----------------------------------------------

    fn print_mission_started(&self, state: &mut RendererState<W>, ev: &Event) {
        let phases = data_int(&ev.data, "phases");
        let mode = data_str(&ev.data, "execution_mode");
        let _ = writeln!(
            state.w,
            "{}{}▸ mission started:{} {phases} phase(s), {mode}\n",
            self.c.bold, self.c.cyan, self.c.reset
        );
    }

    fn print_mission_completed(&self, state: &mut RendererState<W>, ev: &Event) {
        let dur = data_str(&ev.data, "duration");
        let artifacts = data_int(&ev.data, "artifacts");
        let _ = write!(
            state.w,
            "\n{}{}✔ mission completed{} in {dur}",
            self.c.bold, self.c.green, self.c.reset
        );
        if artifacts > 0 {
            let _ = write!(state.w, " ({artifacts} artifact(s))");
        }
        let _ = writeln!(state.w);
        self.print_phase_summary(state);
    }

    fn print_mission_failed(&self, state: &mut RendererState<W>, ev: &Event) {
        let dur = data_str(&ev.data, "duration");
        let err_msg = data_str(&ev.data, "error");
        let _ = writeln!(
            state.w,
            "\n{}{}✘ mission failed{} in {dur}: {err_msg}",
            self.c.bold, self.c.red, self.c.reset
        );
        self.print_phase_summary(state);
    }

    fn print_mission_cancelled(&self, state: &mut RendererState<W>, ev: &Event) {
        let dur = data_str(&ev.data, "duration");
        let _ = writeln!(
            state.w,
            "\n{}{}⊘ mission cancelled{} after {dur}",
            self.c.bold, self.c.yellow, self.c.reset
        );
        self.print_phase_summary(state);
    }

    fn print_phase_summary(&self, state: &mut RendererState<W>) {
        if state.completed + state.failed + state.skipped == 0 {
            return;
        }
        let _ = writeln!(
            state.w,
            "  phases: {} completed, {} failed, {} skipped",
            state.completed, state.failed, state.skipped
        );
    }

    // --- phase lifecycle --------------------------------------------------

    fn print_phase_started(&self, state: &mut RendererState<W>, ev: &Event) {
        let name = data_str(&ev.data, "name");
        let persona = data_str(&ev.data, "persona");

        state
            .phase_starts
            .insert(ev.phase_id.clone(), ev.timestamp_unix_nanos);
        state.phase_meta.insert(
            ev.phase_id.clone(),
            PhaseMeta {
                name: name.clone(),
                persona: persona.clone(),
            },
        );

        let _ = writeln!(
            state.w,
            "{}{}▶ {name}{} {}({persona}){}",
            self.c.bold, self.c.blue, self.c.reset, self.c.dim, self.c.reset
        );
    }

    fn print_phase_completed(&self, state: &mut RendererState<W>, ev: &Event) {
        state.completed += 1;
        let meta = state
            .phase_meta
            .get(&ev.phase_id)
            .cloned()
            .unwrap_or_default();
        let dur = phase_duration(state, ev);
        let retries = data_int(&ev.data, "retries");

        let suffix = if retries > 0 {
            format!(" ({retries} retries)")
        } else {
            String::new()
        };

        let _ = writeln!(
            state.w,
            "{}{}✔ {}{} completed in {dur}{suffix}",
            self.c.bold, self.c.green, meta.name, self.c.reset
        );
    }

    fn print_phase_failed(&self, state: &mut RendererState<W>, ev: &Event) {
        state.failed += 1;
        let meta = state
            .phase_meta
            .get(&ev.phase_id)
            .cloned()
            .unwrap_or_default();
        let dur = phase_duration(state, ev);
        let err_msg = data_str(&ev.data, "error");

        let name = if meta.name.is_empty() {
            ev.phase_id.clone()
        } else {
            meta.name
        };

        let _ = writeln!(
            state.w,
            "{}{}✘ {name}{} failed after {dur}: {err_msg}",
            self.c.bold, self.c.red, self.c.reset
        );
    }

    fn print_phase_skipped(&self, state: &mut RendererState<W>, ev: &Event) {
        state.skipped += 1;
        let name = data_str(&ev.data, "name");
        let reason = data_str(&ev.data, "reason");
        let name = if name.is_empty() {
            ev.phase_id.clone()
        } else {
            name
        };

        let _ = write!(
            state.w,
            "{}{}⊘ {name}{} skipped",
            self.c.bold, self.c.yellow, self.c.reset
        );
        if !reason.is_empty() {
            let _ = write!(state.w, ": {reason}");
        }
        let _ = writeln!(state.w);
    }

    fn print_phase_retrying(&self, state: &mut RendererState<W>, ev: &Event) {
        let meta = state
            .phase_meta
            .get(&ev.phase_id)
            .cloned()
            .unwrap_or_default();
        let attempt = data_int(&ev.data, "attempt");
        let backoff = data_str(&ev.data, "backoff");
        let err_msg = data_str(&ev.data, "error");

        let name = if meta.name.is_empty() {
            ev.phase_id.clone()
        } else {
            meta.name
        };

        let _ = writeln!(
            state.w,
            "{}{}↻ {name}{} attempt {attempt} failed ({err_msg}), retrying in {backoff}",
            self.c.bold, self.c.yellow, self.c.reset
        );
    }

    // --- worker output ------------------------------------------------

    fn print_worker_output(&self, state: &mut RendererState<W>, ev: &Event) {
        let chunk = data_str(&ev.data, "chunk");
        if chunk.is_empty() {
            return;
        }
        let kind = data_str(&ev.data, "event_kind");
        if kind == "tool_use" || kind == "tool_result" {
            let tool = data_str(&ev.data, "tool_name");
            if !tool.is_empty() {
                let _ = writeln!(state.w, "  {}⚙ {tool}{}", self.c.dim, self.c.reset);
            }
            return;
        }
        for line in chunk.split('\n') {
            if line.is_empty() {
                continue;
            }
            let _ = writeln!(state.w, "  {}{line}{}", self.c.dim, self.c.reset);
        }
    }

    // --- git events -----------------------------------------------------

    fn print_git_worktree_created(&self, state: &mut RendererState<W>, ev: &Event) {
        let branch = data_str(&ev.data, "branch");
        let _ = writeln!(
            state.w,
            "{}{}▸ git:{} worktree created (branch: {branch})",
            self.c.bold, self.c.magenta, self.c.reset
        );
    }

    fn print_git_committed(&self, state: &mut RendererState<W>, ev: &Event) {
        let sha = data_str(&ev.data, "sha");
        let sha = truncate_at_byte_boundary(&sha, 8);
        let _ = writeln!(
            state.w,
            "{}{}▸ git:{} committed {sha}",
            self.c.bold, self.c.magenta, self.c.reset
        );
    }

    fn print_git_pr_created(&self, state: &mut RendererState<W>, ev: &Event) {
        let url = data_str(&ev.data, "pr_url");
        let _ = writeln!(
            state.w,
            "{}{}▸ git:{} PR created → {url}",
            self.c.bold, self.c.magenta, self.c.reset
        );
    }

    // --- system -----------------------------------------------------------

    fn print_file_overlap_detected(&self, state: &mut RendererState<W>, ev: &Event) {
        let file = data_str(&ev.data, "file");
        let severity = data_str(&ev.data, "severity");
        let mut phases = Vec::new();
        if let Some(Value::Array(raw)) = ev.data.get("phases") {
            for item in raw {
                if let Value::String(s) = item {
                    phases.push(s.clone());
                }
            }
        }
        let _ = writeln!(
            state.w,
            "{}{}⚠ file_overlap [{severity}]:{} {file} — phases: {}",
            self.c.bold,
            self.c.yellow,
            self.c.reset,
            phases.join(", ")
        );
    }

    fn print_system_error(&self, state: &mut RendererState<W>, ev: &Event) {
        let err_msg = data_str(&ev.data, "error");
        let _ = writeln!(
            state.w,
            "{}{}✘ system error:{} {err_msg}",
            self.c.bold, self.c.red, self.c.reset
        );
    }

    // --- verbose raw event output ------------------------------------------

    fn print_raw_event(&self, state: &mut RendererState<W>, ev: &Event) {
        let ts = format_hms_millis(ev.timestamp_unix_nanos);
        let seq = format!("{:>4}", ev.sequence);

        let mut parts = Vec::new();
        if !ev.phase_id.is_empty() {
            parts.push(format!("phase={}", ev.phase_id));
        }
        if !ev.worker_id.is_empty() {
            parts.push(format!("worker={}", ev.worker_id));
        }
        for (key, value) in &ev.data {
            parts.push(format!("{key}={}", go_value_to_string(value)));
        }

        let ctx = if parts.is_empty() {
            String::new()
        } else {
            format!("  {}", parts.join(" "))
        };

        let type_str = &ev.event_type;
        let _ = writeln!(
            state.w,
            "{}{ts} {seq} {type_str:<30}{ctx}{}",
            self.c.dim, self.c.reset
        );
    }
}

impl TerminalRenderer<Stderr> {
    /// Ported from `NewTerminalRenderer` (`terminal.go:75-84`): creates a
    /// renderer writing to stderr with a TTY-detected palette.
    #[must_use]
    pub fn new_stderr(verbose: bool) -> Self {
        let stderr = std::io::stderr();
        let palette = detect_palette(&stderr);
        Self::new(stderr, palette, verbose)
    }
}

// --- helpers ----------------------------------------------------------

/// Ported from `phaseDuration` (`terminal.go:397-402`).
fn phase_duration<W: Write>(state: &RendererState<W>, ev: &Event) -> String {
    match state.phase_starts.get(&ev.phase_id) {
        Some(&start) => {
            let diff_nanos = ev.timestamp_unix_nanos.saturating_sub(start);
            format_go_duration_seconds(rounded_seconds(diff_nanos))
        }
        None => "?".to_owned(),
    }
}

const NANOS_PER_SEC: i64 = 1_000_000_000;

/// Rounds a nanosecond duration to the nearest whole second using Go's
/// `Duration.Round` algorithm (ties round away from zero), then returns the
/// result in whole seconds. Go: `ev.Timestamp.Sub(start).Round(time.Second)`
/// (`terminal.go:399`).
fn rounded_seconds(diff_nanos: i64) -> i64 {
    go_round_duration(diff_nanos, NANOS_PER_SEC) / NANOS_PER_SEC
}

/// Direct port of Go's `time.Duration.Round` (`$GOROOT/src/time/time.go`).
/// Saturating arithmetic replaces Go's implicit `int64`/`uint64` wraparound
/// at the extreme clamp branches per `PORTING.md` §3 ("Integer conversions").
fn go_round_duration(d: i64, m: i64) -> i64 {
    if m <= 0 {
        return d;
    }
    let r = d % m;
    if d < 0 {
        let r = -r;
        if less_than_half(r, m) {
            return d + r;
        }
        let d1 = d.saturating_sub(m).saturating_add(r);
        if d1 < d {
            return d1;
        }
        return i64::MIN;
    }
    if less_than_half(r, m) {
        return d - r;
    }
    let d1 = d.saturating_add(m).saturating_sub(r);
    if d1 > d {
        return d1;
    }
    i64::MAX
}

fn less_than_half(x: i64, y: i64) -> bool {
    x.unsigned_abs().saturating_mul(2) < y.unsigned_abs()
}

/// Formats a whole-second count the way Go's `Duration.String()` does after
/// rounding to `time.Second` (hours/minutes omitted when zero; seconds
/// always present). Examples: `0` → `"0s"`, `5` → `"5s"`, `330` → `"5m30s"`,
/// `3661` → `"1h1m1s"`.
fn format_go_duration_seconds(total_seconds: i64) -> String {
    if total_seconds == 0 {
        return "0s".to_owned();
    }
    let negative = total_seconds < 0;
    let mut remaining = total_seconds.unsigned_abs();
    let hours = remaining / 3600;
    remaining %= 3600;
    let minutes = remaining / 60;
    let seconds = remaining % 60;

    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if hours > 0 {
        out.push_str(&format!("{hours}h{minutes}m{seconds}s"));
    } else if minutes > 0 {
        out.push_str(&format!("{minutes}m{seconds}s"));
    } else {
        out.push_str(&format!("{seconds}s"));
    }
    out
}

/// Formats the time-of-day portion of a Unix-epoch-nanosecond timestamp as
/// `HH:MM:SS.mmm`. See the module-level divergence note: this renders UTC,
/// not the host's local zone, because no local-timezone dependency exists
/// in this workspace.
fn format_hms_millis(nanos_since_epoch: i64) -> String {
    let secs_since_epoch = nanos_since_epoch.div_euclid(NANOS_PER_SEC);
    let nanos_of_sec = nanos_since_epoch.rem_euclid(NANOS_PER_SEC);
    let millis = nanos_of_sec / 1_000_000;
    let secs_of_day = secs_since_epoch.rem_euclid(86_400);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{hour:02}:{minute:02}:{second:02}.{millis:03}")
}

/// Ported from `dataStr` (`terminal.go:404-417`).
fn data_str(data: &BTreeMap<String, Value>, key: &str) -> String {
    data.get(key).map_or_else(String::new, go_value_to_string)
}

/// Ported from `dataInt` (`terminal.go:419-437`). Go's `switch` accepts
/// `int`, `int64`, and `float64`, truncating floats toward zero; JSON
/// numbers decode to `serde_json::Number`, so both integer and float
/// representations are tried the same way.
fn data_int(data: &BTreeMap<String, Value>, key: &str) -> i64 {
    match data.get(key) {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        _ => 0,
    }
}

/// Ported from `strVal` (`terminal.go:439-449`).
fn str_val(m: &serde_json::Map<String, Value>, key: &str) -> String {
    m.get(key).map_or_else(String::new, go_value_to_string)
}

/// Approximates Go's `fmt.Sprintf("%v", v)` for a decoded JSON value: bare
/// strings are returned unquoted, other values render in Go's default `%v`
/// shape (`[a b c]` for slices, `map[k:v]` for maps). Object/array key order
/// uses the workspace's `BTreeMap` determinism upgrade (`PORTING.md` §3,
/// "Map iteration order") rather than Go's randomized map order.
fn go_value_to_string(v: &Value) -> String {
    match v {
        Value::Null => "<nil>".to_owned(),
        Value::Bool(b) => b.to_string(),
        // `serde_json::Number::to_string` preserves the JSON literal shape
        // (e.g. "3.0"), but Go's `%v` on a float64 uses `%g`, which drops a
        // trailing ".0" (`fmt.Sprintf("%v", 3.0)` == "3"); route true floats
        // through `f64`'s `Display`, which matches for whole-number and
        // ordinary-magnitude values.
        Value::Number(n) if n.is_f64() => n.as_f64().unwrap_or_default().to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(go_value_to_string).collect();
            format!("[{}]", inner.join(" "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}:{}", go_value_to_string(v)))
                .collect();
            format!("map[{}]", inner.join(" "))
        }
    }
}

/// Truncates a string to at most `max_bytes` bytes, snapping down to the
/// nearest UTF-8 char boundary. Go's `summary[:77]` (`terminal.go:152`) and
/// `sha[:8]` (`terminal.go:336`) slice by raw byte index and can split a
/// multi-byte rune, which `unsafe_code = "forbid"` rules out reproducing
/// exactly in Rust; this is the flagged one-line divergence (only reachable
/// for non-ASCII input beyond the truncation point).
fn truncate_at_byte_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::io::Cursor;
    use std::sync::Arc;
    use std::thread;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn no_color_renderer() -> TerminalRenderer<Cursor<Vec<u8>>> {
        TerminalRenderer::new(Cursor::new(Vec::new()), Palette::default(), false)
    }

    fn take_output(
        renderer: TerminalRenderer<Cursor<Vec<u8>>>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let state = renderer.state.into_inner()?;
        Ok(String::from_utf8(state.w.into_inner())?)
    }

    /// A regular (non-terminal) file, standing in for Go's `os.CreateTemp`
    /// substrate — real TTYs are not constructible in either test suite
    /// (see the Go source comment at `terminal_test.go:23-25`). Unlike Go's
    /// `defer os.Remove(tmpfile.Name())`, the caller must remove the
    /// returned path itself (see call sites) — no `tempfile` dependency is
    /// approved for this crate.
    fn non_terminal_file(name: &str) -> std::io::Result<(File, std::path::PathBuf)> {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "orchestrator_render_test_{name}_{}_{}",
            std::process::id(),
            name.len()
        ));
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)?;
        Ok((file, path))
    }

    // TestColorDetectionTTY (terminal_test.go:15-32) — despite the name, the
    // Go test itself only exercises a temp file (a real TTY cannot be
    // constructed in a test), so both this and the next test assert the
    // same non-TTY behavior, matching Go's own scope.
    #[test]
    fn color_detection_tty() -> TestResult {
        let (file, path) = non_terminal_file("tty")?;
        let palette = detect_palette(&file);
        assert_eq!(palette.reset, "");
        assert_eq!(palette.bold, "");
        drop(file);
        std::fs::remove_file(path)?;
        Ok(())
    }

    // TestColorDetectionNonTTY (terminal_test.go:35-50)
    #[test]
    fn color_detection_non_tty() -> TestResult {
        let (file, path) = non_terminal_file("pipe")?;
        let palette = detect_palette(&file);
        assert_eq!(palette.reset, "");
        assert_eq!(palette.bold, "");
        assert_eq!(palette.cyan, "");
        drop(file);
        std::fs::remove_file(path)?;
        Ok(())
    }

    // TestOutputFormat_DecomposeStarted (terminal_test.go:53-84)
    #[test]
    fn output_format_decompose_started() -> TestResult {
        let renderer = no_color_renderer();
        let mut data = BTreeMap::new();
        data.insert(
            "task_summary".to_owned(),
            Value::String("implement user authentication".to_owned()),
        );
        renderer.emit(&Event {
            event_type: event_type::DECOMPOSE_STARTED.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 0,
            phase_id: String::new(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("decomposing"));
        assert!(output.contains("implement user authentication"));
        assert!(output.contains('▸'));
        Ok(())
    }

    // TestOutputFormat_DecomposeStarted_Truncation (terminal_test.go:87-119)
    #[test]
    fn output_format_decompose_started_truncation() -> TestResult {
        let renderer = no_color_renderer();
        let long_summary = "a".repeat(100);
        let mut data = BTreeMap::new();
        data.insert(
            "task_summary".to_owned(),
            Value::String(long_summary.clone()),
        );
        renderer.emit(&Event {
            event_type: event_type::DECOMPOSE_STARTED.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 0,
            phase_id: String::new(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("..."));
        assert!(!output.contains(&long_summary));
        Ok(())
    }

    // TestOutputFormat_MissionStarted (terminal_test.go:122-153)
    #[test]
    fn output_format_mission_started() -> TestResult {
        let renderer = no_color_renderer();
        let mut data = BTreeMap::new();
        data.insert("phases".to_owned(), Value::from(5));
        data.insert(
            "execution_mode".to_owned(),
            Value::String("sequential".to_owned()),
        );
        renderer.emit(&Event {
            event_type: event_type::MISSION_STARTED.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 0,
            phase_id: String::new(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("mission started"));
        assert!(output.contains("5 phase"));
        assert!(output.contains("sequential"));
        Ok(())
    }

    // TestOutputFormat_PhaseStarted (terminal_test.go:156-188)
    #[test]
    fn output_format_phase_started() -> TestResult {
        let renderer = no_color_renderer();
        let mut data = BTreeMap::new();
        data.insert("name".to_owned(), Value::String("implement".to_owned()));
        data.insert(
            "persona".to_owned(),
            Value::String("senior-backend-engineer".to_owned()),
        );
        renderer.emit(&Event {
            event_type: event_type::PHASE_STARTED.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 0,
            phase_id: "phase-1".to_owned(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("implement"));
        assert!(output.contains("senior-backend-engineer"));
        assert!(output.contains('▶'));
        Ok(())
    }

    // TestOutputFormat_PhaseCompleted (terminal_test.go:191-230)
    #[test]
    fn output_format_phase_completed() -> TestResult {
        let renderer = no_color_renderer();
        let now: i64 = 10_000_000_000;
        {
            let mut state = lock_unpoisoned(&renderer.state);
            state
                .phase_starts
                .insert("phase-1".to_owned(), now - 5 * NANOS_PER_SEC);
            state.phase_meta.insert(
                "phase-1".to_owned(),
                PhaseMeta {
                    name: "implement".to_owned(),
                    persona: "engineer".to_owned(),
                },
            );
        }
        let mut data = BTreeMap::new();
        data.insert("retries".to_owned(), Value::from(0));
        renderer.emit(&Event {
            event_type: event_type::PHASE_COMPLETED.to_owned(),
            timestamp_unix_nanos: now,
            sequence: 0,
            phase_id: "phase-1".to_owned(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("implement"));
        assert!(output.contains("completed"));
        assert!(output.contains("5s"));
        assert!(output.contains('✔'));
        Ok(())
    }

    // TestOutputFormat_PhaseCompleted_WithRetries (terminal_test.go:233-262)
    #[test]
    fn output_format_phase_completed_with_retries() -> TestResult {
        let renderer = no_color_renderer();
        let now: i64 = 10_000_000_000;
        {
            let mut state = lock_unpoisoned(&renderer.state);
            state.phase_starts.insert("phase-1".to_owned(), now);
            state.phase_meta.insert(
                "phase-1".to_owned(),
                PhaseMeta {
                    name: "test".to_owned(),
                    persona: "engineer".to_owned(),
                },
            );
        }
        let mut data = BTreeMap::new();
        data.insert("retries".to_owned(), Value::from(2));
        renderer.emit(&Event {
            event_type: event_type::PHASE_COMPLETED.to_owned(),
            timestamp_unix_nanos: now + NANOS_PER_SEC,
            sequence: 0,
            phase_id: "phase-1".to_owned(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("2 retries"));
        Ok(())
    }

    // TestOutputFormat_PhaseFailed (terminal_test.go:265-300)
    #[test]
    fn output_format_phase_failed() -> TestResult {
        let renderer = no_color_renderer();
        let now: i64 = 10_000_000_000;
        {
            let mut state = lock_unpoisoned(&renderer.state);
            state.phase_starts.insert("phase-1".to_owned(), now);
            state.phase_meta.insert(
                "phase-1".to_owned(),
                PhaseMeta {
                    name: "implement".to_owned(),
                    persona: "engineer".to_owned(),
                },
            );
        }
        let mut data = BTreeMap::new();
        data.insert(
            "error".to_owned(),
            Value::String("connection timeout".to_owned()),
        );
        renderer.emit(&Event {
            event_type: event_type::PHASE_FAILED.to_owned(),
            timestamp_unix_nanos: now + 10 * NANOS_PER_SEC,
            sequence: 0,
            phase_id: "phase-1".to_owned(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("failed"));
        assert!(output.contains("connection timeout"));
        assert!(output.contains('✘'));
        Ok(())
    }

    // TestOutputFormat_PhaseSkipped (terminal_test.go:303-335)
    #[test]
    fn output_format_phase_skipped() -> TestResult {
        let renderer = no_color_renderer();
        let mut data = BTreeMap::new();
        data.insert("name".to_owned(), Value::String("review".to_owned()));
        data.insert(
            "reason".to_owned(),
            Value::String("no code changes".to_owned()),
        );
        renderer.emit(&Event {
            event_type: event_type::PHASE_SKIPPED.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 0,
            phase_id: "phase-2".to_owned(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("skipped"));
        assert!(output.contains("no code changes"));
        assert!(output.contains('⊘'));
        Ok(())
    }

    // TestOutputFormat_MissionCompleted (terminal_test.go:338-383)
    #[test]
    fn output_format_mission_completed() -> TestResult {
        let renderer = no_color_renderer();
        {
            let mut state = lock_unpoisoned(&renderer.state);
            state.completed = 3;
            state.failed = 0;
            state.skipped = 1;
        }
        let mut data = BTreeMap::new();
        data.insert("duration".to_owned(), Value::String("2m30s".to_owned()));
        data.insert("artifacts".to_owned(), Value::from(2));
        renderer.emit(&Event {
            event_type: event_type::MISSION_COMPLETED.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 0,
            phase_id: String::new(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("mission completed"));
        assert!(output.contains("2m30s"));
        assert!(output.contains("2 artifact"));
        assert!(output.contains("3 completed"));
        assert!(output.contains("1 skipped"));
        assert!(output.contains('✔'));
        Ok(())
    }

    // TestOutputFormat_MissionFailed (terminal_test.go:386-420)
    #[test]
    fn output_format_mission_failed() -> TestResult {
        let renderer = no_color_renderer();
        {
            let mut state = lock_unpoisoned(&renderer.state);
            state.completed = 1;
            state.failed = 1;
        }
        let mut data = BTreeMap::new();
        data.insert("duration".to_owned(), Value::String("1m15s".to_owned()));
        data.insert(
            "error".to_owned(),
            Value::String("worker process exited unexpectedly".to_owned()),
        );
        renderer.emit(&Event {
            event_type: event_type::MISSION_FAILED.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 0,
            phase_id: String::new(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("mission failed"));
        assert!(output.contains("worker process exited"));
        assert!(output.contains('✘'));
        Ok(())
    }

    // TestVerboseMode (terminal_test.go:423-465)
    #[test]
    fn verbose_mode() -> TestResult {
        let renderer = TerminalRenderer::new(Cursor::new(Vec::new()), Palette::default(), true);
        let mut data = BTreeMap::new();
        data.insert("name".to_owned(), Value::String("test".to_owned()));
        renderer.emit(&Event {
            event_type: event_type::PHASE_STARTED.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 42,
            phase_id: "phase-1".to_owned(),
            worker_id: "worker-1".to_owned(),
            data,
        });
        let output = take_output(renderer)?;
        let lines: Vec<&str> = output.split('\n').collect();
        assert!(lines.len() >= 2);
        let has_raw_event = lines
            .iter()
            .any(|line| line.contains("phase.started") && line.contains("phase=phase-1"));
        assert!(has_raw_event);
        Ok(())
    }

    // TestVerboseMode_ShowsTimestamp (terminal_test.go:468-493)
    #[test]
    fn verbose_mode_shows_timestamp() -> TestResult {
        let renderer = TerminalRenderer::new(Cursor::new(Vec::new()), Palette::default(), true);
        let mut data = BTreeMap::new();
        data.insert("phases".to_owned(), Value::from(3));
        renderer.emit(&Event {
            event_type: event_type::MISSION_STARTED.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 0,
            phase_id: String::new(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains(':'));
        Ok(())
    }

    // TestNonVerboseMode (terminal_test.go:496-524)
    #[test]
    fn non_verbose_mode() -> TestResult {
        let renderer = no_color_renderer();
        let mut data = BTreeMap::new();
        data.insert("name".to_owned(), Value::String("implement".to_owned()));
        data.insert("persona".to_owned(), Value::String("engineer".to_owned()));
        renderer.emit(&Event {
            event_type: event_type::PHASE_STARTED.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 0,
            phase_id: "phase-1".to_owned(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(!output.contains("phase="));
        Ok(())
    }

    // TestWorkerOutput_ToolUse (terminal_test.go:527-557)
    #[test]
    fn worker_output_tool_use() -> TestResult {
        let renderer = no_color_renderer();
        let mut data = BTreeMap::new();
        data.insert(
            "event_kind".to_owned(),
            Value::String("tool_use".to_owned()),
        );
        data.insert("tool_name".to_owned(), Value::String("Read".to_owned()));
        data.insert(
            "chunk".to_owned(),
            Value::String("some very long tool output that should be abbreviated".to_owned()),
        );
        renderer.emit(&Event {
            event_type: event_type::WORKER_OUTPUT.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 0,
            phase_id: String::new(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("Read"));
        assert!(!output.contains("some very long"));
        Ok(())
    }

    // TestWorkerOutput_TextContent (terminal_test.go:560-589)
    #[test]
    fn worker_output_text_content() -> TestResult {
        let renderer = no_color_renderer();
        let mut data = BTreeMap::new();
        data.insert("event_kind".to_owned(), Value::String("text".to_owned()));
        data.insert(
            "chunk".to_owned(),
            Value::String("this is worker output\nwith multiple lines".to_owned()),
        );
        renderer.emit(&Event {
            event_type: event_type::WORKER_OUTPUT.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 0,
            phase_id: String::new(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("this is worker output"));
        assert!(output.contains("multiple lines"));
        Ok(())
    }

    // TestWorkerOutput_EmptyChunk (terminal_test.go:592-617)
    #[test]
    fn worker_output_empty_chunk() -> TestResult {
        let renderer = no_color_renderer();
        let mut data = BTreeMap::new();
        data.insert("event_kind".to_owned(), Value::String("text".to_owned()));
        data.insert("chunk".to_owned(), Value::String(String::new()));
        renderer.emit(&Event {
            event_type: event_type::WORKER_OUTPUT.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 0,
            phase_id: String::new(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.is_empty());
        Ok(())
    }

    // TestGitEvents (terminal_test.go:620-680)
    #[test]
    fn git_events() -> TestResult {
        let cases: Vec<(&str, BTreeMap<String, Value>, Vec<&str>)> = vec![
            (
                event_type::GIT_WORKTREE_CREATED,
                BTreeMap::from([(
                    "branch".to_owned(),
                    Value::String("feature/auth".to_owned()),
                )]),
                vec!["git:", "worktree created", "feature/auth"],
            ),
            (
                event_type::GIT_COMMITTED,
                BTreeMap::from([(
                    "sha".to_owned(),
                    Value::String("1a2b3c4d5e6f7g8h9i0j".to_owned()),
                )]),
                vec!["git:", "committed", "1a2b3c4d"],
            ),
            (
                event_type::GIT_PR_CREATED,
                BTreeMap::from([(
                    "pr_url".to_owned(),
                    Value::String("https://github.com/user/repo/pull/42".to_owned()),
                )]),
                vec!["git:", "PR created", "https://github.com"],
            ),
        ];

        for (kind, data, should_match) in cases {
            let renderer = no_color_renderer();
            renderer.emit(&Event {
                event_type: kind.to_owned(),
                timestamp_unix_nanos: 0,
                sequence: 0,
                phase_id: String::new(),
                worker_id: String::new(),
                data,
            });
            let output = take_output(renderer)?;
            for expected in should_match {
                assert!(
                    output.contains(expected),
                    "output should contain {expected:?}"
                );
            }
        }
        Ok(())
    }

    // TestSystemError (terminal_test.go:683-710)
    #[test]
    fn system_error() -> TestResult {
        let renderer = no_color_renderer();
        let mut data = BTreeMap::new();
        data.insert(
            "error".to_owned(),
            Value::String("database connection failed".to_owned()),
        );
        renderer.emit(&Event {
            event_type: event_type::SYSTEM_ERROR.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 0,
            phase_id: String::new(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("system error"));
        assert!(output.contains("database connection failed"));
        Ok(())
    }

    // TestPhaseSummary_NoPhases (terminal_test.go:713-742)
    #[test]
    fn phase_summary_no_phases() -> TestResult {
        let renderer = no_color_renderer();
        let mut data = BTreeMap::new();
        data.insert("duration".to_owned(), Value::String("1s".to_owned()));
        data.insert("artifacts".to_owned(), Value::from(0));
        renderer.emit(&Event {
            event_type: event_type::MISSION_COMPLETED.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 0,
            phase_id: String::new(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(!output.contains("phases:"));
        Ok(())
    }

    // TestPhaseDuration_Formatting (terminal_test.go:745-775)
    #[test]
    fn phase_duration_formatting() -> TestResult {
        let renderer = no_color_renderer();
        let now: i64 = 100 * NANOS_PER_SEC;
        let start = now - (5 * 60 + 30) * NANOS_PER_SEC;
        {
            let mut state = lock_unpoisoned(&renderer.state);
            state.phase_starts.insert("phase-1".to_owned(), start);
            state.phase_meta.insert(
                "phase-1".to_owned(),
                PhaseMeta {
                    name: "long-task".to_owned(),
                    persona: "worker".to_owned(),
                },
            );
        }
        let mut data = BTreeMap::new();
        data.insert("retries".to_owned(), Value::from(0));
        renderer.emit(&Event {
            event_type: event_type::PHASE_COMPLETED.to_owned(),
            timestamp_unix_nanos: now,
            sequence: 0,
            phase_id: "phase-1".to_owned(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("5m30s") || output.contains("330s"));
        Ok(())
    }

    // TestPhaseMetadata_FallbackToPhaseID (terminal_test.go:778-808)
    #[test]
    fn phase_metadata_fallback_to_phase_id() -> TestResult {
        let renderer = no_color_renderer();
        let now: i64 = 10 * NANOS_PER_SEC;
        {
            let mut state = lock_unpoisoned(&renderer.state);
            state.phase_starts.insert("phase-abc123".to_owned(), now);
        }
        let mut data = BTreeMap::new();
        data.insert("error".to_owned(), Value::String("timeout".to_owned()));
        renderer.emit(&Event {
            event_type: event_type::PHASE_FAILED.to_owned(),
            timestamp_unix_nanos: now + NANOS_PER_SEC,
            sequence: 0,
            phase_id: "phase-abc123".to_owned(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("phase-abc123"));
        Ok(())
    }

    // TestThreadSafety (terminal_test.go:811-849)
    #[test]
    fn thread_safety() -> TestResult {
        let renderer = Arc::new(no_color_renderer());
        let mut handles = Vec::new();
        for idx in 0..10 {
            let renderer = Arc::clone(&renderer);
            handles.push(
                thread::Builder::new()
                    .name(format!("render-test-emit-{idx}"))
                    .spawn(move || {
                        let mut data = BTreeMap::new();
                        data.insert("name".to_owned(), Value::String("phase".to_owned()));
                        data.insert("persona".to_owned(), Value::String("worker".to_owned()));
                        renderer.emit(&Event {
                            event_type: event_type::PHASE_STARTED.to_owned(),
                            timestamp_unix_nanos: 0,
                            sequence: 0,
                            phase_id: format!("phase-{idx}"),
                            worker_id: String::new(),
                            data,
                        });
                    })?,
            );
        }
        for handle in handles {
            handle
                .join()
                .map_err(|_| "renderer thread panicked".to_owned())?;
        }
        let renderer = Arc::into_inner(renderer).ok_or("renderer still shared")?;
        let output = take_output(renderer)?;
        assert!(!output.is_empty());
        Ok(())
    }

    // TestDecomposeCompleted_PhaseTable (terminal_test.go:852-899)
    #[test]
    fn decompose_completed_phase_table() -> TestResult {
        let renderer = no_color_renderer();
        let mut data = BTreeMap::new();
        data.insert("phase_count".to_owned(), Value::from(3));
        data.insert(
            "execution_mode".to_owned(),
            Value::String("sequential".to_owned()),
        );
        data.insert(
            "phases".to_owned(),
            Value::Array(vec![
                serde_json::json!({"name": "plan", "persona": "architect"}),
                serde_json::json!({"name": "implement", "persona": "engineer"}),
                serde_json::json!({"name": "review", "persona": "reviewer"}),
            ]),
        );
        renderer.emit(&Event {
            event_type: event_type::DECOMPOSE_COMPLETED.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 0,
            phase_id: String::new(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("3 phase"));
        assert!(output.contains("plan") && output.contains("architect"));
        assert!(output.contains("implement") && output.contains("engineer"));
        assert!(output.contains("review") && output.contains("reviewer"));
        assert!(output.contains("1.") && output.contains("2.") && output.contains("3."));
        Ok(())
    }

    // TestPhaseRetrying (terminal_test.go:902-943)
    #[test]
    fn phase_retrying() -> TestResult {
        let renderer = no_color_renderer();
        {
            let mut state = lock_unpoisoned(&renderer.state);
            state.phase_meta.insert(
                "phase-1".to_owned(),
                PhaseMeta {
                    name: "deploy".to_owned(),
                    persona: "devops".to_owned(),
                },
            );
        }
        let mut data = BTreeMap::new();
        data.insert("attempt".to_owned(), Value::from(2));
        data.insert(
            "error".to_owned(),
            Value::String("service unavailable".to_owned()),
        );
        data.insert("backoff".to_owned(), Value::String("5s".to_owned()));
        renderer.emit(&Event {
            event_type: event_type::PHASE_RETRYING.to_owned(),
            timestamp_unix_nanos: 0,
            sequence: 0,
            phase_id: "phase-1".to_owned(),
            worker_id: String::new(),
            data,
        });
        let output = take_output(renderer)?;
        assert!(output.contains("deploy"));
        assert!(output.contains("attempt 2"));
        assert!(output.contains("service unavailable"));
        assert!(output.contains("5s"));
        assert!(output.contains('↻'));
        Ok(())
    }

    #[test]
    fn go_duration_formatting_matches_known_values() {
        assert_eq!(format_go_duration_seconds(0), "0s");
        assert_eq!(format_go_duration_seconds(5), "5s");
        assert_eq!(format_go_duration_seconds(30), "30s");
        assert_eq!(format_go_duration_seconds(90), "1m30s");
        assert_eq!(format_go_duration_seconds(330), "5m30s");
        assert_eq!(format_go_duration_seconds(3600), "1h0m0s");
        assert_eq!(format_go_duration_seconds(3661), "1h1m1s");
        assert_eq!(format_go_duration_seconds(-30), "-30s");
    }

    // Rust-only: no ported Go test exercises a `Value::Number` that decodes
    // to a `float64` (Go's `%v` on 3.0 == "3", not "3.0" —
    // `fmt.Sprintf("%v", 3.0)` verified against Go stdlib semantics).
    #[test]
    fn go_value_to_string_formats_whole_floats_without_go_zero() {
        let data = BTreeMap::from([("elapsed".to_owned(), serde_json::json!(3.0))]);
        assert_eq!(data_str(&data, "elapsed"), "3");
        let data = BTreeMap::from([("elapsed".to_owned(), serde_json::json!(3.5))]);
        assert_eq!(data_str(&data, "elapsed"), "3.5");
        let data = BTreeMap::from([("count".to_owned(), serde_json::json!(3))]);
        assert_eq!(data_str(&data, "count"), "3");
    }
}
