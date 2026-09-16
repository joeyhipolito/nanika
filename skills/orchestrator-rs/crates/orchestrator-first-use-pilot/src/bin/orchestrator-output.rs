//! Explicit opt-in command output compaction. This helper grants no execution authority.
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::time::Instant;

use serde::Serialize;
use serde_json::json;

const REVISION: &str = "portal-output/v1";
const CONTROLS_REVISION: &str = "portal-controls/v1";
const BUDGET: usize = 16 * 1024;
const PREVIEW: usize = 512;
const HELP: &str = "orchestrator-output --portal off|on [--output-cap off|on] --log NEW-FILE -- COMMAND [ARG...]\nBoth modes save merged stdout/stderr in a fresh private log and replay after command completion.\n--output-cap controls this helper's own output compaction only; it does not toggle any\nfuture automatic provider integration. When omitted, the output cap follows --portal.\nPortal off is always raw and bypasses summarization; --portal off --output-cap on is refused\nbefore any command runs or artifact is created (contradictory request). Portal on with\n--output-cap off records the requested Portal on but returns raw bytes (effective cap off).\nBoth raw and capped modes keep identical full-log fidelity; raw helper output is replayed\nverbatim only after the command has completed. Fresh NEW-FILE.portal.json records requested\nand effective portal/output-cap state, mode source, command status and delivery.\nNo shell is implied. Requires foreground commands; no daemon ownership; no worker auto-wiring.";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Off,
    On,
}

struct Options {
    requested_portal: Mode,
    requested_output_cap: Option<Mode>,
    effective_output_cap: Mode,
    mode_source: &'static str,
    log: PathBuf,
    command: Vec<OsString>,
}

fn parse_mode(value: &OsString, what: &str) -> Result<Mode> {
    match value.to_str() {
        Some("off") => Ok(Mode::Off),
        Some("on") => Ok(Mode::On),
        _ => Err(format!("unsupported {what}; use off or on").into()),
    }
}

// Portal off is always raw; explicit --output-cap on for --portal off is contradictory
// and must be refused before any command runs or artifact is created.
fn resolve_output_cap(portal: Mode, cap: Option<Mode>) -> Result<(Mode, &'static str)> {
    match (portal, cap) {
        (Mode::Off, Some(Mode::On)) => Err(
            "contradictory request: --portal off cannot be combined with --output-cap on".into(),
        ),
        (Mode::Off, Some(Mode::Off)) => Ok((Mode::Off, "explicit-output-cap")),
        (Mode::Off, None) => Ok((Mode::Off, "portal-default")),
        (Mode::On, Some(cap)) => Ok((cap, "explicit-output-cap")),
        (Mode::On, None) => Ok((Mode::On, "portal-default")),
    }
}

fn parse(args: Vec<OsString>) -> Result<Options> {
    if args.len() < 6 || args[0] != "--portal" {
        return Err(HELP.into());
    }
    let portal = parse_mode(&args[1], "Portal mode")?;
    let (requested_output_cap, log_index) = if args[2] == "--output-cap" {
        if args.len() < 8 {
            return Err(HELP.into());
        }
        (Some(parse_mode(&args[3], "--output-cap value")?), 4)
    } else {
        (None, 2)
    };
    if args[log_index] != "--log" {
        return Err(HELP.into());
    }
    let dash_index = log_index + 2;
    if args.len() <= dash_index || args[dash_index] != "--" {
        return Err(HELP.into());
    }
    let (effective_output_cap, mode_source) = resolve_output_cap(portal, requested_output_cap)?;
    let log = std::path::absolute(PathBuf::from(&args[log_index + 1]))?;
    let label = log.to_str().ok_or("log path must be UTF-8")?;
    // Leave enough JSON budget for an exact retrievable artifact reference.
    if label.len() > 2048 || label.chars().any(char::is_control) {
        return Err("log path exceeds 2048 bytes or contains control characters".into());
    }
    Ok(Options {
        requested_portal: portal,
        requested_output_cap,
        effective_output_cap,
        mode_source,
        log,
        command: args[dash_index + 1..].to_vec(),
    })
}

#[derive(Clone, Serialize)]
struct Excerpt {
    line: u64,
    byte_offset: u64,
    bytes: u64,
    preview_bytes: usize,
    preview: String,
    partial: bool,
}

#[derive(Default, Serialize)]
struct Summary {
    lines: u64,
    failure_lines: u64,
    summary_lines: u64,
    failure_excerpts_omitted: u64,
    summary_excerpts_omitted: u64,
    failures: Vec<Excerpt>,
    summaries: Vec<Excerpt>,
    tail: VecDeque<Excerpt>,
}

#[derive(Default)]
struct Line {
    bytes: u64,
    preview: Vec<u8>,
    overlap: Vec<u8>,
    failure: bool,
    summary: bool,
}
impl Line {
    fn push(&mut self, bytes: &[u8]) {
        self.bytes += bytes.len() as u64;
        self.preview
            .extend(bytes.iter().take(PREVIEW - self.preview.len()));
        let mut search = std::mem::take(&mut self.overlap);
        search.extend(bytes.iter().map(u8::to_ascii_lowercase));
        self.failure |= [b"error".as_slice(), b"fail", b"panicked", b"fatal"]
            .iter()
            .any(|needle| search.windows(needle.len()).any(|s| s == *needle));
        self.summary |= [b"test result:".as_slice(), b"pass"]
            .iter()
            .any(|needle| search.windows(needle.len()).any(|s| s == *needle));
        self.overlap
            .extend_from_slice(&search[search.len().saturating_sub(16)..]);
    }
    fn finish(self, out: &mut Summary, offset: u64) {
        out.lines += 1;
        let excerpt = Excerpt {
            line: out.lines,
            byte_offset: offset,
            bytes: self.bytes,
            preview_bytes: self.preview.len(),
            partial: self.bytes > self.preview.len() as u64,
            preview: String::from_utf8_lossy(&self.preview).into_owned(),
        };
        if self.failure {
            out.failure_lines += 1;
            if out.failures.len() < 12 {
                out.failures.push(excerpt.clone());
            }
        }
        if self.summary {
            out.summary_lines += 1;
            if out.summaries.len() < 8 {
                out.summaries.push(excerpt.clone());
            }
        }
        if out.tail.len() == 6 {
            out.tail.pop_front();
        }
        out.tail.push_back(excerpt);
    }
}

fn summarize(reader: impl Read) -> io::Result<Summary> {
    let mut reader = BufReader::with_capacity(8192, reader);
    let mut out = Summary::default();
    let mut line = Line::default();
    let mut offset = 0;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        let count = chunk
            .iter()
            .position(|b| *b == b'\n')
            .map_or(chunk.len(), |n| n + 1);
        let complete = chunk[count - 1] == b'\n';
        line.push(&chunk[..count]);
        reader.consume(count);
        if complete {
            let bytes = line.bytes;
            std::mem::take(&mut line).finish(&mut out, offset);
            offset += bytes;
        }
    }
    if line.bytes != 0 {
        line.finish(&mut out, offset);
    }
    Ok(out)
}

fn compact(options: &Options, exit: i32, bytes: u64, mut summary: Summary) -> Result<Vec<u8>> {
    loop {
        summary.failure_excerpts_omitted = summary.failure_lines - summary.failures.len() as u64;
        summary.summary_excerpts_omitted = summary.summary_lines - summary.summaries.len() as u64;
        let mut encoded = serde_json::to_vec(&json!({
            "revision": REVISION, "controls_revision": CONTROLS_REVISION,
            "requested_portal": options.requested_portal, "effective_portal": options.effective_output_cap,
            "requested_output_cap": options.requested_output_cap, "effective_output_cap": options.effective_output_cap,
            "mode_source": options.mode_source,
            "command_exit_code": exit, "full_log": options.log, "log_bytes": bytes,
            "response_budget_bytes": BUDGET, "summary": summary,
            "retrieval": "Read full_log using byte_offset/bytes or line numbers. Previews are lossy UTF-8; full log is raw. Heuristic keyword matches do not determine command success.",
            "scope": "foreground command snapshot; excerpts may omit lines and partial-line content"
        }))?;
        encoded.push(b'\n');
        if encoded.len() <= BUDGET {
            return Ok(encoded);
        }
        if summary.tail.pop_front().is_some() {
            continue;
        }
        if summary.summaries.pop().is_some() {
            continue;
        }
        if summary.failures.pop().is_some() {
            continue;
        }
        return Err("artifact reference cannot fit response budget".into());
    }
}

fn fresh(path: &std::path::Path) -> io::Result<File> {
    OpenOptions::new()
        .create_new(true)
        .read(true)
        .append(true)
        .mode(0o600)
        .open(path)
}
fn named_identity(file: &File, path: &std::path::Path) -> Result<()> {
    let named = std::fs::symlink_metadata(path)?;
    let opened = file.metadata()?;
    if !named.is_file()
        || named.file_type().is_symlink()
        || (named.dev(), named.ino()) != (opened.dev(), opened.ino())
    {
        return Err("artifact path no longer names the original regular file".into());
    }
    Ok(())
}
fn record(file: &mut File, path: &std::path::Path, data: &serde_json::Value) -> Result<()> {
    named_identity(file, path)?;
    file.set_len(0)?;
    serde_json::to_writer(&mut *file, data)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    named_identity(file, path)?;
    Ok(())
}
fn status_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}
fn unchanged(a: &Metadata, b: &Metadata) -> bool {
    (
        a.dev(),
        a.ino(),
        a.len(),
        a.mtime(),
        a.mtime_nsec(),
        a.ctime(),
        a.ctime_nsec(),
    ) == (
        b.dev(),
        b.ino(),
        b.len(),
        b.mtime(),
        b.mtime_nsec(),
        b.ctime(),
        b.ctime_nsec(),
    )
}

#[derive(Default)]
struct Delivered<W> {
    inner: W,
    bytes: u64,
}
impl<W: Write> Write for Delivered<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(bytes)?;
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn execute(options: &Options, output: impl Write) -> Result<i32> {
    let mut sidecar = options.log.as_os_str().to_os_string();
    sidecar.push(".portal.json");
    // Reserve both names before execution. On admission failure, keep any newly
    // reserved artifact rather than race an unlink against another process.
    let sidecar = PathBuf::from(sidecar);
    let mut metadata = fresh(&sidecar)?;
    let mut receipt = json!({
        "revision":REVISION,"controls_revision":CONTROLS_REVISION,
        "requested_portal":options.requested_portal,"effective_portal":options.effective_output_cap,
        "requested_output_cap":options.requested_output_cap,"effective_output_cap":options.effective_output_cap,
        "mode_source":options.mode_source,
        "full_log":options.log,"state":"admitting","command_exit_code":null,"returned_bytes":0
    });
    record(&mut metadata, &sidecar, &receipt)?;
    let mut log = fresh(&options.log)?;
    receipt["state"] = json!("running");
    record(&mut metadata, &sidecar, &receipt)?;
    let started = Instant::now();
    let status = Command::new(&options.command[0])
        .args(&options.command[1..])
        .stdin(Stdio::inherit())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?)
        .status();
    let exit = match status {
        Ok(status) => status_code(status),
        Err(error) => {
            receipt["spawn_error"] = json!(error.to_string());
            127
        }
    };
    receipt["command_exit_code"] = json!(exit);
    receipt["command_elapsed_ms"] = json!(started.elapsed().as_millis());
    receipt["state"] = json!("command_finished");
    record(&mut metadata, &sidecar, &receipt)?;
    let mut delivered = Delivered {
        inner: output,
        bytes: 0,
    };
    let result = (|| -> Result<()> {
        named_identity(&log, &options.log)?;
        log.sync_all()?;
        let before = log.metadata()?;
        receipt["log_bytes"] = json!(before.len());
        log.seek(SeekFrom::Start(0))?;
        let reader = (&mut log).take(before.len());
        if options.effective_output_cap == Mode::On {
            let summary = summarize(reader)?;
            if !unchanged(&before, &log.metadata()?) {
                return Err("log changed during summary; use foreground commands".into());
            }
            named_identity(&log, &options.log)?;
            let bytes = compact(options, exit, before.len(), summary)?;
            receipt["response_budget_bytes"] = json!(BUDGET);
            delivered.write_all(&bytes)?;
        } else {
            let count = io::copy(&mut { reader }, &mut delivered)?;
            if count != before.len() {
                return Err("log shortened during replay".into());
            }
        }
        delivered.flush()?;
        named_identity(&log, &options.log)?;
        if !unchanged(&before, &log.metadata()?) {
            return Err("log changed during replay; use foreground commands".into());
        }
        Ok(())
    })();
    receipt["returned_bytes"] = json!(delivered.bytes);
    receipt["elapsed_ms"] = json!(started.elapsed().as_millis());
    receipt["state"] = json!(if result.is_ok() {
        "completed"
    } else {
        "delivery_failed"
    });
    if let Err(error) = &result {
        receipt["delivery_error"] = json!(error.to_string());
    }
    record(&mut metadata, &sidecar, &receipt)?;
    result?;
    Ok(exit)
}
fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--help" {
        println!("{HELP}");
        return;
    }
    let options = match parse(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    match execute(&options, io::stdout().lock()) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("orchestrator-output: {error}");
            std::process::exit(125);
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "Test failures should panic with their source location"
)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "portal-output-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn options(&self, mode: Mode, args: &[&str]) -> Options {
            self.options_cap(mode, None, args)
        }
        fn options_cap(&self, portal: Mode, cap: Option<Mode>, args: &[&str]) -> Options {
            let (effective_output_cap, mode_source) = resolve_output_cap(portal, cap).unwrap();
            Options {
                requested_portal: portal,
                requested_output_cap: cap,
                effective_output_cap,
                mode_source,
                log: self.0.join("full.log"),
                command: args.iter().map(OsString::from).collect(),
            }
        }
        fn receipt(&self) -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(self.0.join("full.log.portal.json")).unwrap())
                .unwrap()
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn off_preserves_raw_binary_and_merged_stderr() {
        let t = Temp::new();
        let o = t.options(
            Mode::Off,
            &[
                "/bin/sh",
                "-c",
                "printf '\\377\\000out'; printf 'err' >&2; exit 7",
            ],
        );
        let mut bytes = Vec::new();
        assert_eq!(execute(&o, &mut bytes).unwrap(), 7);
        assert_eq!(bytes, b"\xff\0outerr");
        assert_eq!(std::fs::read(&o.log).unwrap(), bytes);
        assert_eq!(t.receipt()["effective_portal"], "off");
        assert_eq!(std::fs::metadata(&o.log).unwrap().mode() & 0o777, 0o600);
    }
    #[test]
    fn on_preserves_failures_and_success_summary() {
        let t = Temp::new();
        let o = t.options(
            Mode::On,
            &[
                "/bin/sh",
                "-c",
                "printf 'noise\nerror[E1]: broken\ntest result: FAILED\n'; exit 42",
            ],
        );
        let mut bytes = Vec::new();
        assert_eq!(execute(&o, &mut bytes).unwrap(), 42);
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["summary"]["failure_lines"], 2);
        assert_eq!(v["summary"]["summary_lines"], 1);
        assert_eq!(v["command_exit_code"], 42);
        assert_eq!(t.receipt()["returned_bytes"], bytes.len());
    }
    #[test]
    fn many_errors_have_explicit_overflow_with_byte_locations() {
        let t = Temp::new();
        let o = t.options(Mode::On, &["unused"]);
        let data = "error: failure\n".repeat(10000);
        let s = summarize(data.as_bytes()).unwrap();
        let bytes = compact(&o, 1, data.len() as u64, s).unwrap();
        assert!(bytes.len() <= BUDGET);
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["summary"]["failure_lines"], 10000);
        assert_eq!(v["summary"]["failure_excerpts_omitted"], 9988);
        assert_eq!(v["summary"]["failures"][1]["byte_offset"], 15);
    }
    #[test]
    fn huge_unterminated_line_detects_keyword_across_buffer_boundary() {
        let mut data = vec![b'x'; 8190];
        data.extend_from_slice(b"error");
        data.extend(vec![b'x'; 1000000]);
        let s = summarize(data.as_slice()).unwrap();
        assert_eq!(s.failure_lines, 1);
        assert_eq!(s.lines, 1);
        assert_eq!(s.failures[0].preview_bytes, PREVIEW);
        assert!(s.failures[0].partial);
        assert_eq!(s.failures[0].bytes, data.len() as u64);
    }
    #[test]
    fn json_escape_expansion_stays_bounded() {
        let t = Temp::new();
        let o = t.options(Mode::On, &["unused"]);
        let data = ("error ".to_owned() + &"\0".repeat(510) + "\n").repeat(30);
        let bytes = compact(
            &o,
            1,
            data.len() as u64,
            summarize(data.as_bytes()).unwrap(),
        )
        .unwrap();
        assert!(bytes.len() <= BUDGET);
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["summary"]["failure_excerpts_omitted"].as_u64().unwrap() > 18);
    }
    #[test]
    fn command_arguments_are_not_shell_expanded() {
        let t = Temp::new();
        let o = t.options(
            Mode::Off,
            &["/usr/bin/printf", "%s", "$(exit 9); $HOME `exit 8`"],
        );
        let mut bytes = Vec::new();
        assert_eq!(execute(&o, &mut bytes).unwrap(), 0);
        assert_eq!(bytes, b"$(exit 9); $HOME `exit 8`");
    }
    #[test]
    fn collisions_do_not_run_command_or_overwrite() {
        for sidecar in [false, true] {
            let t = Temp::new();
            let o = t.options(
                Mode::Off,
                &["/usr/bin/touch", t.0.join("executed").to_str().unwrap()],
            );
            let path = if sidecar {
                t.0.join("full.log.portal.json")
            } else {
                o.log.clone()
            };
            std::fs::write(&path, b"existing").unwrap();
            assert!(execute(&o, Vec::new()).is_err());
            assert!(!t.0.join("executed").exists());
            assert_eq!(std::fs::read(path).unwrap(), b"existing");
        }
    }
    #[test]
    fn symlink_log_is_not_followed() {
        let t = Temp::new();
        let o = t.options(Mode::Off, &["/usr/bin/true"]);
        let target = t.0.join("target");
        std::os::unix::fs::symlink(&target, &o.log).unwrap();
        assert!(execute(&o, Vec::new()).is_err());
        assert!(!target.exists());
    }
    #[test]
    fn missing_executable_is_127_and_recorded() {
        let t = Temp::new();
        let o = t.options(Mode::On, &["/definitely/not/a/command"]);
        assert_eq!(execute(&o, Vec::new()).unwrap(), 127);
        assert!(t.receipt()["spawn_error"].is_string());
    }
    #[test]
    fn child_signal_is_preserved() {
        let t = Temp::new();
        let o = t.options(Mode::On, &["/bin/sh", "-c", "kill -TERM $$"]);
        assert_eq!(execute(&o, Vec::new()).unwrap(), 143);
        assert_eq!(t.receipt()["command_exit_code"], 143);
    }
    #[test]
    fn delivery_failure_retains_command_exit_status() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let t = Temp::new();
        let o = t.options(Mode::Off, &["/bin/sh", "-c", "printf hello; exit 9"]);
        assert!(execute(&o, Broken).is_err());
        assert_eq!(t.receipt()["command_exit_code"], 9);
        assert_eq!(t.receipt()["state"], "delivery_failed");
        assert_eq!(std::fs::read(&o.log).unwrap(), b"hello");
    }
    #[test]
    fn replaced_log_path_cannot_report_successful_delivery() {
        let t = Temp::new();
        let path = t.0.join("full.log");
        let o = t.options(
            Mode::On,
            &[
                "/bin/sh",
                "-c",
                "printf original; rm \"$1\"; printf replacement > \"$1\"",
                "sh",
                path.to_str().unwrap(),
            ],
        );
        let mut bytes = Vec::new();
        assert!(execute(&o, &mut bytes).is_err());
        assert!(bytes.is_empty());
        assert_eq!(t.receipt()["state"], "delivery_failed");
        assert_eq!(t.receipt()["command_exit_code"], 0);
    }
    #[test]
    fn replaced_receipt_cannot_report_successful_delivery() {
        let t = Temp::new();
        let path = t.0.join("full.log.portal.json");
        let o = t.options(
            Mode::Off,
            &[
                "/bin/sh",
                "-c",
                "rm \"$1\"; printf replacement > \"$1\"",
                "sh",
                path.to_str().unwrap(),
            ],
        );
        assert!(execute(&o, Vec::new()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"replacement");
    }
    #[test]
    fn unsupported_or_missing_mode_is_rejected() {
        for args in [
            vec!["--portal", "auto", "--log", "x", "--", "true"],
            vec!["--log", "x", "--", "true"],
            vec!["--portal", "off", "--log", "x", "--"],
        ] {
            assert!(parse(args.into_iter().map(OsString::from).collect()).is_err());
        }
    }

    #[test]
    fn old_shape_defaults_output_cap_to_portal() {
        for (mode, expect_effective) in [("off", Mode::Off), ("on", Mode::On)] {
            let o = parse(
                vec!["--portal", mode, "--log", "x", "--", "true"]
                    .into_iter()
                    .map(OsString::from)
                    .collect(),
            )
            .unwrap();
            assert!(o.requested_output_cap.is_none());
            assert_eq!(o.effective_output_cap, expect_effective);
            assert_eq!(o.mode_source, "portal-default");
        }
    }
    #[test]
    fn new_shape_combinations_and_contradiction() {
        let o = parse(
            vec![
                "--portal",
                "on",
                "--output-cap",
                "off",
                "--log",
                "x",
                "--",
                "true",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        )
        .unwrap();
        assert_eq!(o.requested_portal, Mode::On);
        assert_eq!(o.requested_output_cap, Some(Mode::Off));
        assert_eq!(o.effective_output_cap, Mode::Off);
        assert_eq!(o.mode_source, "explicit-output-cap");

        let o = parse(
            vec![
                "--portal",
                "on",
                "--output-cap",
                "on",
                "--log",
                "x",
                "--",
                "true",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        )
        .unwrap();
        assert_eq!(o.effective_output_cap, Mode::On);
        assert_eq!(o.mode_source, "explicit-output-cap");

        let o = parse(
            vec![
                "--portal",
                "off",
                "--output-cap",
                "off",
                "--log",
                "x",
                "--",
                "true",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        )
        .unwrap();
        assert_eq!(o.effective_output_cap, Mode::Off);
        assert_eq!(o.mode_source, "explicit-output-cap");
    }
    #[test]
    fn contradictory_portal_off_output_cap_on_is_refused_without_execution() {
        let t = Temp::new();
        let log = t.0.join("full.log");
        let marker = t.0.join("executed");
        let args = vec![
            "--portal",
            "off",
            "--output-cap",
            "on",
            "--log",
            log.to_str().unwrap(),
            "--",
            "/usr/bin/touch",
            marker.to_str().unwrap(),
        ];
        assert!(parse(args.into_iter().map(OsString::from).collect()).is_err());
        assert!(!t.0.join("executed").exists());
        assert!(!t.0.join("full.log").exists());
        assert!(!t.0.join("full.log.portal.json").exists());
    }
    #[test]
    fn portal_on_output_cap_off_returns_raw_bytes_with_metadata() {
        let t = Temp::new();
        let o = t.options_cap(
            Mode::On,
            Some(Mode::Off),
            &[
                "/bin/sh",
                "-c",
                "printf '\\377\\000out'; printf 'err' >&2; exit 5",
            ],
        );
        let mut bytes = Vec::new();
        assert_eq!(execute(&o, &mut bytes).unwrap(), 5);
        assert_eq!(bytes, b"\xff\0outerr");
        let receipt = t.receipt();
        assert_eq!(receipt["requested_portal"], "on");
        assert_eq!(receipt["effective_portal"], "off");
        assert_eq!(receipt["requested_output_cap"], "off");
        assert_eq!(receipt["effective_output_cap"], "off");
        assert_eq!(receipt["mode_source"], "explicit-output-cap");
        assert_eq!(receipt["controls_revision"], "portal-controls/v1");
    }
    #[test]
    fn portal_on_output_cap_on_applies_cap_with_metadata() {
        let t = Temp::new();
        let o = t.options_cap(
            Mode::On,
            Some(Mode::On),
            &[
                "/bin/sh",
                "-c",
                "printf 'error boom\ntest result: ok\n'; exit 3",
            ],
        );
        let mut bytes = Vec::new();
        assert_eq!(execute(&o, &mut bytes).unwrap(), 3);
        assert!(bytes.len() <= BUDGET);
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["requested_portal"], "on");
        assert_eq!(v["effective_portal"], "on");
        assert_eq!(v["requested_output_cap"], "on");
        assert_eq!(v["effective_output_cap"], "on");
        assert_eq!(v["mode_source"], "explicit-output-cap");
        assert_eq!(v["controls_revision"], "portal-controls/v1");
        assert_eq!(v["command_exit_code"], 3);
    }
    #[test]
    fn parser_rejects_unknown_duplicate_and_missing_values() {
        for args in [
            vec![
                "--portal", "on", "--bogus", "on", "--log", "x", "--", "true",
            ],
            vec![
                "--portal",
                "on",
                "--output-cap",
                "on",
                "--output-cap",
                "off",
                "--log",
                "x",
                "--",
                "true",
            ],
            vec!["--portal", "on", "--output-cap", "--log", "x", "--", "true"],
            vec![
                "--portal",
                "on",
                "--output-cap",
                "bogus",
                "--log",
                "x",
                "--",
                "true",
            ],
            vec!["--portal", "on", "--output-cap", "on", "--log", "x", "--"],
            vec!["--portal", "on", "--output-cap", "on", "x", "--", "true"],
        ] {
            assert!(
                parse(args.into_iter().map(OsString::from).collect()).is_err(),
                "expected rejection"
            );
        }
    }
}
