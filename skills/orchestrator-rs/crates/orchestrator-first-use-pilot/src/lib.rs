//! Experimental supplied-source review and isolated coding pilot.
//! Review uses Claude by default and Codex by opt-in; coding uses Codex by
//! default and admits Claude only through an explicit restricted 2.1.269 path.
//! Uses ExecutorRegistry/ExecutionContext and ProcessSupervisor for one bounded
//! attempt. Native provider parsers require terminal success and agent text.
//! Ambient executable version gates are not production enrollment or attestation.

mod authored_cycle;
mod cancellation;
mod codex;
mod cycle;
mod durable;
mod features;
mod observe;
mod portal;
mod portal_command;
mod progress;
mod routing;
mod snapshot;
mod usage;
mod usage_codex;
#[cfg(test)]
mod usage_codex_tests;
mod usage_live;
mod usage_runtime;
mod view;

use std::ffi::OsString;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use orchestrator_exec::{
    AttemptOutcome, Cancellation, Clock, EffectBudget, EffectReceipt, EffectRequest, EffectService,
    EffectServiceError, EffectServiceErrorKind, EventReceipt, EventSink, EventSinkError,
    EventSinkErrorKind, ExecutionContext, ExecutionRequest, ExecutionRequestDraft,
    ExecutorRegistry, ProcessBudget, ProcessExitStatus, ProcessPreflight, ProcessPurpose,
    ProcessReceipt, ProcessRequest, ProcessService, ProcessServiceError, ProcessServiceErrorKind,
    ProcessTerminationReceipt, RuntimeFamily, WatchdogDecision, WatchdogPolicy, WorkerEventDraft,
    WorkerIdentity,
};
use orchestrator_process::{
    CancellationToken, ProcessReport, ProcessSpec, ProcessSupervisor, ProcessTermination,
};
use orchestrator_provider_claude::{
    ClaudeCodingExecutor, ClaudeProvider, ClaudeProviderConfig,
    FIRST_USE_PILOT_CLAUDE_CODE_VERSION, FIRST_USE_PILOT_CLAUDE_MAX_TURNS,
};
use serde_json::{Value, json};
use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::iterator::{Handle as SignalHandle, Signals};
use thiserror::Error;

/// Runtime opt-in. Only the exact value `1` admits a run.
pub const OPT_IN_ENV: &str = "NANIKA_RUST_FIRST_USE_PILOT";

/// Claude passes the prompt as one `-p` argument capped at 8 KiB, including
/// its `Task: ` prefix. Larger prompts are refused, never truncated.
pub const MAX_PROMPT_BYTES: u64 = 8 * 1024 - 64;
const CODE_OBJECTIVE_PREFIX: &str = "Implement the requested change in the current disposable workspace. Treat the task below as a coding specification only. Do not run tests, linters, builds, formatters, package managers, or Git commands; verification belongs to the operator. Do not read or write outside the current workspace. Make only the requested code changes.\n\nTask:\n";
const MAX_CODE_PROMPT_BYTES: u64 = 8 * 1024 - CODE_OBJECTIVE_PREFIX.len() as u64;

const RESULT_SCHEMA: &str = "nanika.rust-first-use-pilot.v1";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_TIMEOUT: Duration = Duration::from_secs(1800);
const MAX_STALL_WINDOW: Duration = Duration::from_secs(300);
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const VERSION_PROBE_MAX_OUTPUT: usize = 4096;
const CLAUDE_EXECUTABLE_ID: &str = "claude";
const MISSION_ID: &str = "rust-first-use-pilot";
/// Ambient values the Claude CLI needs to locate its own login and helpers.
/// No API key or `CLAUDE_*` variable is forwarded.
const INHERITED_ENVIRONMENT: [&str; 5] = ["HOME", "USER", "LOGNAME", "PATH", "TMPDIR"];

const USAGE: &str = "\
usage: NANIKA_RUST_FIRST_USE_PILOT=1 orchestrator-first-use-pilot review \\
         --prompt-file <file> --output-dir <fresh-dir> \\
         [--runtime <claude|codex>] [--codex <executable>] [--model <model>] [--persona <persona>] [--timeout-secs <1..1800>] [--claude <executable>]

       NANIKA_RUST_FIRST_USE_PILOT=1 orchestrator-first-use-pilot code \\
         --repo <clean-local-repository-root> --prompt-file <file> \\
         --output-dir <fresh-dir> [--runtime <codex|claude>] \\
         [--codex <executable> | --claude <2.1.269-executable>] [--model <model>] \\
         [--persona <persona>] [--timeout-secs <1..1800>]

       NANIKA_RUST_FIRST_USE_PILOT=1 orchestrator-first-use-pilot run \\
         --repo <clean-local-repository-root> \\
         (--task-file <file> | --mission-file <authored-PHASE-file>) \\
         --output-dir <fresh-dir> [--codex <executable>] [--model <model>] \\
         [--persona <persona>] [--timeout-secs <1..1800>] \\
         [--verification-timeout-secs <1..1800>] -- <verification-argv...>

       NANIKA_RUST_FIRST_USE_PILOT=1 orchestrator-first-use-pilot run --durable --repo <repo> \
         --mission-file <authored-PHASE-file> --output-dir <fresh-private-run> \
         [--stop-after-phase <name>] [run options] -- <verification-argv...>

       NANIKA_RUST_FIRST_USE_PILOT=1 orchestrator-first-use-pilot resume \
         --output-dir <owned-durable-run>

       NANIKA_RUST_FIRST_USE_PILOT=1 orchestrator-first-use-pilot status \
         --output-dir <owned-durable-run>

       NANIKA_RUST_FIRST_USE_PILOT=1 orchestrator-first-use-pilot cancel \
         --output-dir <owned-durable-run> --mission <exact-mission-id>

       NANIKA_RUST_FIRST_USE_PILOT=1 orchestrator-first-use-pilot observe \
         --progress-log <regular-file> [--follow] [--format <text|json>]

       NANIKA_RUST_FIRST_USE_PILOT=1 orchestrator-first-use-pilot view \
         --progress-log <regular-file> [--follow]

       orchestrator-first-use-pilot features

Fresh code/review/run accepts repeatable --feature <name=off|on>.
Portal output capping supports ON for standalone Codex code; other ON requests are refused.
OFF receipts distinguish default from explicit requests; durable resume preserves recorded settings.

Experimental pilot: one review, one coding attempt, or a fixed or authored Codex-only local mission cycle.
Run exit 0 requires successful terminal, provider, and post-run gates; a requested durable pause exits 3.
Status is read-only inspection of a strictly validated snapshot of visible recorded journal state;
it checks neither execution liveness nor whether an active writer completed fsync. Valid paused, failed,
completed, cancelled, cancellation-requested, in-progress, or unresolved snapshots exit 0;
unstable, unsafe, or corrupt durable state exits 1;
invalid arguments or missing opt-in exit 2.
";

const EXIT_OK: u8 = 0;
const EXIT_FAILED: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_PAUSED: u8 = 3;

#[derive(Debug, Error)]
pub enum PilotError {
    #[error("{0}")]
    Usage(String),
    #[error("refusing to run: {OPT_IN_ENV}=1 is required for this experimental pilot")]
    NotOptedIn,
    #[error("prompt file {path}: {reason}")]
    Prompt { path: PathBuf, reason: String },
    #[error("output directory {path}: {reason}")]
    OutputDirectory { path: PathBuf, reason: String },
    #[error("source repository {path}: {reason}")]
    Repository { path: PathBuf, reason: String },
    #[error("runtime version probe failed: {0}")]
    VersionProbe(String),
    #[error("runtime reports unsupported version {observed:?}")]
    VersionMismatch { observed: String },
    #[error("pilot composition failed: {0}")]
    Composition(String),
    #[error("writing pilot artifact {path}: {source}")]
    Artifact {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PilotCommand {
    Review,
    Code,
    Run,
    Resume,
    Status,
    Cancel,
    Observe,
    View,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PilotOptions {
    pub command: PilotCommand,
    pub prompt_file: PathBuf,
    pub output_dir: PathBuf,
    pub repo: Option<PathBuf>,
    pub model: String,
    pub persona: Option<String>,
    pub timeout: Duration,
    pub claude_executable: OsString,
    pub runtime: String,
    pub codex_executable: OsString,
    pub verification_argv: Vec<OsString>,
    pub verification_timeout: Duration,
    pub authored_mission: bool,
    pub durable: bool,
    pub stop_after_phase: Option<String>,
    pub mission_id: Option<String>,
    pub(crate) feature_requests: features::Requests,
    pub(crate) progress_log: Option<PathBuf>,
    pub(crate) observe_follow: bool,
    pub(crate) observe_format: observe::OutputFormat,
}

impl PilotOptions {
    fn executable(&self) -> OsString {
        if self.runtime == "codex" {
            self.codex_executable.clone()
        } else {
            self.claude_executable.clone()
        }
    }
    fn required_version(&self) -> &'static str {
        if self.runtime == "codex" {
            codex::VERSION
        } else {
            FIRST_USE_PILOT_CLAUDE_CODE_VERSION
        }
    }
}

/// Parses the experimental `review` and `code` commands.
pub fn parse_arguments<I, S>(arguments: I) -> Result<PilotOptions, PilotError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let arguments: Vec<String> = arguments.into_iter().map(Into::into).collect();
    let usage = |message: &str| PilotError::Usage(message.to_owned());
    let command = match arguments.first().map(String::as_str) {
        Some("review") => PilotCommand::Review,
        Some("code") => PilotCommand::Code,
        Some("run") => PilotCommand::Run,
        Some("resume") => PilotCommand::Resume,
        Some("status") => PilotCommand::Status,
        Some("cancel") => PilotCommand::Cancel,
        Some("observe") => PilotCommand::Observe,
        Some("view") => PilotCommand::View,
        Some(other) => return Err(usage(&format!("unknown command {other:?}"))),
        None => {
            return Err(usage(
                "missing command: review, code, run, resume, status, cancel, observe, or view",
            ));
        }
    };
    let mut feature_requests = features::Requests::default();
    let mut prompt_file = None;
    let mut output_dir = None;
    let mut repo = None;
    let mut model = String::new();
    let mut persona = None;
    let mut timeout = DEFAULT_TIMEOUT;
    let mut claude_executable = OsString::from(CLAUDE_EXECUTABLE_ID);
    let mut runtime = "claude".to_owned();
    let mut codex_executable = OsString::from("codex");
    let mut verification_argv = Vec::new();
    let mut verification_timeout = DEFAULT_TIMEOUT;
    let mut authored_mission = false;
    let mut durable = false;
    let mut stop_after_phase = None;
    let mut mission_id = None;
    let mut progress_log = None;
    let mut observe_follow = false;
    let mut observe_format = observe::OutputFormat::Text;
    let mut observe_format_explicit = false;
    let mut output_explicit = false;
    let mut runtime_explicit = false;
    let mut claude_explicit = false;
    let mut model_explicit = false;
    let mut codex_explicit = false;
    let mut timeout_explicit = false;
    let mut verification_timeout_explicit = false;
    let mut rest = arguments[1..].iter();
    while let Some(flag) = rest.next() {
        if flag == "--" {
            if command != PilotCommand::Run {
                return Err(usage("verification argv is available only with run"));
            }
            verification_argv.extend(rest.map(OsString::from));
            break;
        }
        let mut value = || {
            rest.next()
                .cloned()
                .ok_or_else(|| usage(&format!("{flag} requires a value")))
        };
        match flag.as_str() {
            "--feature" => {
                if !matches!(
                    command,
                    PilotCommand::Code | PilotCommand::Review | PilotCommand::Run
                ) {
                    return Err(usage(
                        "--feature is available only with code, review, or run; resume preserves recorded settings",
                    ));
                }
                feature_requests
                    .request(&value()?)
                    .map_err(PilotError::Usage)?;
            }
            "--prompt-file" => {
                if command == PilotCommand::Run {
                    return Err(usage("run requires --task-file, not --prompt-file"));
                }
                prompt_file = Some(PathBuf::from(value()?));
            }
            "--task-file" => {
                if command != PilotCommand::Run {
                    return Err(usage("--task-file is available only with run"));
                }
                if prompt_file.is_some() {
                    return Err(usage(
                        "run accepts exactly one of --task-file or --mission-file",
                    ));
                }
                prompt_file = Some(PathBuf::from(value()?));
            }
            "--mission-file" => {
                if command != PilotCommand::Run {
                    return Err(usage("--mission-file is available only with run"));
                }
                if prompt_file.is_some() {
                    return Err(usage(
                        "run accepts exactly one of --task-file or --mission-file",
                    ));
                }
                prompt_file = Some(PathBuf::from(value()?));
                authored_mission = true;
            }
            "--output-dir" => {
                if output_explicit && matches!(command, PilotCommand::Status | PilotCommand::Cancel)
                {
                    return Err(usage("command does not accept duplicate --output-dir"));
                }
                output_explicit = true;
                output_dir = Some(PathBuf::from(value()?));
            }
            "--mission" => {
                if command != PilotCommand::Cancel || mission_id.is_some() {
                    return Err(usage("--mission is accepted exactly once with cancel"));
                }
                let value = value()?;
                if value.is_empty() {
                    return Err(usage("--mission must be nonempty"));
                }
                mission_id = Some(value);
            }
            "--progress-log" => {
                if !matches!(command, PilotCommand::Observe | PilotCommand::View)
                    || progress_log.is_some()
                {
                    return Err(usage(
                        "--progress-log is accepted exactly once with observe or view",
                    ));
                }
                progress_log = Some(PathBuf::from(value()?));
            }
            "--follow" => {
                if !matches!(command, PilotCommand::Observe | PilotCommand::View) || observe_follow
                {
                    return Err(usage(
                        "--follow is accepted exactly once with observe or view",
                    ));
                }
                observe_follow = true;
            }
            "--format" => {
                if command != PilotCommand::Observe || observe_format_explicit {
                    return Err(usage("--format is accepted exactly once with observe"));
                }
                observe_format_explicit = true;
                observe_format = observe::OutputFormat::parse(&value()?)
                    .ok_or_else(|| usage("--format must be text or json"))?;
            }
            "--durable" => {
                if command != PilotCommand::Run {
                    return Err(usage("--durable is available only with run"));
                }
                durable = true;
            }
            "--stop-after-phase" => {
                if command != PilotCommand::Run {
                    return Err(usage("--stop-after-phase is available only with run"));
                }
                let name = value()?;
                if name.is_empty() {
                    return Err(usage("--stop-after-phase must be nonempty"));
                }
                stop_after_phase = Some(name);
            }
            "--repo" => repo = Some(PathBuf::from(value()?)),
            "--model" => {
                model_explicit = true;
                model = value()?;
                if model.trim().is_empty() {
                    return Err(usage("--model must be nonempty"));
                }
            }
            "--persona" => {
                let name = value()?;
                if name.trim().is_empty() {
                    return Err(usage("--persona must be nonempty"));
                }
                persona = Some(name);
            }
            "--runtime" => {
                if runtime_explicit {
                    return Err(usage("--runtime is accepted at most once"));
                }
                runtime_explicit = true;
                runtime = value()?;
                if !matches!(runtime.as_str(), "claude" | "codex") {
                    return Err(usage("--runtime must be claude or codex"));
                }
            }
            "--codex" => {
                if codex_explicit {
                    return Err(usage("--codex is accepted at most once"));
                }
                codex_explicit = true;
                let executable = value()?;
                if executable.trim().is_empty() {
                    return Err(usage("--codex must be nonempty"));
                }
                codex_executable = OsString::from(executable);
            }
            "--claude" => {
                if claude_explicit {
                    return Err(usage("--claude is accepted at most once"));
                }
                claude_explicit = true;
                let executable = value()?;
                if executable.trim().is_empty() {
                    return Err(usage("--claude must be nonempty"));
                }
                claude_executable = OsString::from(executable);
            }
            "--timeout-secs" => {
                timeout_explicit = true;
                let raw = value()?;
                let seconds: u64 = raw
                    .parse()
                    .map_err(|_| usage(&format!("--timeout-secs {raw:?} is not an integer")))?;
                timeout = Duration::from_secs(seconds);
                if timeout.is_zero() || timeout > MAX_TIMEOUT {
                    return Err(usage("--timeout-secs must be between 1 and 1800"));
                }
            }
            "--verification-timeout-secs" => {
                verification_timeout_explicit = true;
                if command != PilotCommand::Run {
                    return Err(usage(
                        "--verification-timeout-secs is available only with run",
                    ));
                }
                let raw = value()?;
                let seconds: u64 = raw.parse().map_err(|_| {
                    usage(&format!(
                        "--verification-timeout-secs {raw:?} is not an integer"
                    ))
                })?;
                verification_timeout = Duration::from_secs(seconds);
                if verification_timeout.is_zero() || verification_timeout > MAX_TIMEOUT {
                    return Err(usage(
                        "--verification-timeout-secs must be between 1 and 1800",
                    ));
                }
            }
            other => return Err(usage(&format!("unknown flag {other:?}"))),
        }
    }
    match command {
        PilotCommand::Review if repo.is_some() => {
            return Err(usage("--repo is available only with code"));
        }
        PilotCommand::Code if repo.is_none() => return Err(usage("--repo is required with code")),
        PilotCommand::Code => {
            if !runtime_explicit {
                runtime = "codex".to_owned();
            }
            match runtime.as_str() {
                "claude" if !claude_explicit => {
                    return Err(usage("code --runtime claude requires --claude"));
                }
                "claude" if codex_explicit => {
                    return Err(usage("--codex conflicts with code --runtime claude"));
                }
                "codex" if claude_explicit => {
                    return Err(usage("--claude requires code --runtime claude"));
                }
                _ => {}
            }
        }
        PilotCommand::Run if repo.is_none() => return Err(usage("--repo is required with run")),
        PilotCommand::Run if runtime_explicit || claude_explicit => {
            return Err(usage(
                "run is Codex-only; --runtime and --claude are not accepted",
            ));
        }
        PilotCommand::Run if authored_mission && persona.is_some() => {
            return Err(usage(
                "--persona is not accepted with --mission-file; author PERSONA per phase",
            ));
        }
        PilotCommand::Run if verification_argv.is_empty() => {
            return Err(usage("run requires verification argv after --"));
        }
        PilotCommand::Run => runtime = "codex".to_owned(),
        PilotCommand::Resume
            if repo.is_some()
                || prompt_file.is_some()
                || !verification_argv.is_empty()
                || runtime_explicit
                || claude_explicit
                || model_explicit
                || codex_explicit
                || timeout_explicit
                || verification_timeout_explicit
                || persona.is_some()
                || stop_after_phase.is_some() =>
        {
            return Err(usage("resume accepts only --output-dir"));
        }
        PilotCommand::Resume => {
            runtime = "codex".to_owned();
            durable = true;
            authored_mission = true;
        }
        PilotCommand::Status | PilotCommand::Cancel
            if repo.is_some()
                || prompt_file.is_some()
                || !verification_argv.is_empty()
                || runtime_explicit
                || claude_explicit
                || model_explicit
                || codex_explicit
                || timeout_explicit
                || verification_timeout_explicit
                || persona.is_some()
                || stop_after_phase.is_some()
                || durable
                || authored_mission =>
        {
            return Err(usage("status and cancel accept no execution options"));
        }
        PilotCommand::Status => {}
        PilotCommand::Cancel if mission_id.is_none() => {
            return Err(usage("cancel requires --mission"));
        }
        PilotCommand::Cancel => {}
        PilotCommand::Observe
            if repo.is_some()
                || prompt_file.is_some()
                || !verification_argv.is_empty()
                || runtime_explicit
                || claude_explicit
                || model_explicit
                || codex_explicit
                || timeout_explicit
                || verification_timeout_explicit
                || persona.is_some()
                || stop_after_phase.is_some()
                || durable
                || authored_mission
                || mission_id.is_some()
                || output_explicit =>
        {
            return Err(usage(
                "observe accepts only --progress-log, --follow, and --format",
            ));
        }
        PilotCommand::Observe if progress_log.is_none() => {
            return Err(usage("observe requires --progress-log"));
        }
        PilotCommand::Observe => {}
        PilotCommand::View
            if repo.is_some()
                || prompt_file.is_some()
                || !verification_argv.is_empty()
                || runtime_explicit
                || claude_explicit
                || model_explicit
                || codex_explicit
                || timeout_explicit
                || verification_timeout_explicit
                || persona.is_some()
                || stop_after_phase.is_some()
                || durable
                || authored_mission
                || mission_id.is_some()
                || output_explicit
                || observe_format_explicit =>
        {
            return Err(usage("view accepts only --progress-log and --follow"));
        }
        PilotCommand::View if progress_log.is_none() => {
            return Err(usage("view requires --progress-log"));
        }
        PilotCommand::View => {}
        PilotCommand::Review => {}
    }
    if durable && !authored_mission {
        return Err(usage(
            "durable fixed-task input is not implemented; use --mission-file",
        ));
    }
    if stop_after_phase.is_some() && !durable {
        return Err(usage("--stop-after-phase requires --durable"));
    }
    Ok(PilotOptions {
        command,
        prompt_file: if matches!(
            command,
            PilotCommand::Resume
                | PilotCommand::Status
                | PilotCommand::Cancel
                | PilotCommand::Observe
                | PilotCommand::View
        ) {
            PathBuf::new()
        } else {
            prompt_file.ok_or_else(|| {
                usage(if command == PilotCommand::Run {
                    "run requires exactly one of --task-file or --mission-file"
                } else {
                    "--prompt-file is required"
                })
            })?
        },
        output_dir: if matches!(command, PilotCommand::Observe | PilotCommand::View) {
            PathBuf::new()
        } else {
            output_dir.ok_or_else(|| usage("--output-dir is required"))?
        },
        repo,
        model,
        persona,
        timeout,
        claude_executable,
        runtime,
        codex_executable,
        verification_argv,
        verification_timeout,
        authored_mission,
        durable,
        stop_after_phase,
        mission_id,
        feature_requests,
        progress_log,
        observe_follow,
        observe_format,
    })
}

/// Binary entry point with injected arguments, opt-in value, and streams.
pub fn main_with<I, S>(
    arguments: I,
    opt_in: Option<String>,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> u8
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut arguments = arguments.into_iter();
    let mut lookahead: Vec<String> = Vec::with_capacity(3);
    let help_requested = match arguments.next().map(Into::into) {
        None => false,
        Some(first) if first == "features" => {
            if arguments.next().is_some() {
                let _ = writeln!(error_output, "features accepts no arguments");
                return 2;
            }
            return match serde_json::to_writer_pretty(&mut *output, &features::catalog()) {
                Ok(()) => {
                    if writeln!(output).is_ok() {
                        0
                    } else {
                        1
                    }
                }
                Err(_) => 1,
            };
        }
        Some(first) => {
            lookahead.push(first);
            if matches!(lookahead[0].as_str(), "--help" | "-h") {
                match arguments.next().map(Into::into) {
                    None => true,
                    Some(argument) => {
                        lookahead.push(argument);
                        false
                    }
                }
            } else if lookahead[0] == "help" {
                match arguments.next().map(Into::into) {
                    None => true,
                    Some(command) => {
                        let supported = matches!(
                            command.as_str(),
                            "review"
                                | "code"
                                | "run"
                                | "resume"
                                | "status"
                                | "cancel"
                                | "observe"
                                | "view"
                        );
                        lookahead.push(command);
                        if supported {
                            match arguments.next().map(Into::into) {
                                None => true,
                                Some(argument) => {
                                    lookahead.push(argument);
                                    false
                                }
                            }
                        } else {
                            false
                        }
                    }
                }
            } else if matches!(
                lookahead[0].as_str(),
                "review" | "code" | "run" | "resume" | "status" | "cancel" | "observe" | "view"
            ) {
                match arguments.next().map(Into::into) {
                    Some(help) if matches!(help.as_str(), "--help" | "-h") => {
                        lookahead.push(help);
                        match arguments.next().map(Into::into) {
                            None => true,
                            Some(argument) => {
                                lookahead.push(argument);
                                false
                            }
                        }
                    }
                    Some(argument) => {
                        lookahead.push(argument);
                        false
                    }
                    None => false,
                }
            } else {
                false
            }
        }
    };
    if help_requested {
        let _ = write!(output, "{USAGE}");
        return EXIT_OK;
    }
    let options = match parse_arguments(lookahead.into_iter().chain(arguments.map(Into::into))) {
        Ok(options) => options,
        Err(error) => {
            let _ = writeln!(error_output, "{error}\n\n{USAGE}");
            return EXIT_USAGE;
        }
    };
    if opt_in.as_deref() != Some("1") {
        let _ = writeln!(error_output, "{}", PilotError::NotOptedIn);
        return EXIT_USAGE;
    }
    if options.command == PilotCommand::Status {
        return match durable::status(&options) {
            Ok(report) => {
                let _ = writeln!(output, "{}", report);
                EXIT_OK
            }
            Err(error) => {
                let _ = writeln!(error_output, "pilot refused: {error}");
                EXIT_FAILED
            }
        };
    }
    if options.command == PilotCommand::Cancel {
        return cancellation::run_cli(&options, output, error_output);
    }
    if options.command == PilotCommand::Observe {
        return match observe::run(&options, output) {
            Ok(()) => EXIT_OK,
            Err(error) => {
                let _ = writeln!(
                    error_output,
                    "observer refused: {}",
                    observe::safe_text(&error.to_string())
                );
                EXIT_FAILED
            }
        };
    }
    if options.command == PilotCommand::View {
        return match view::run(&options, output) {
            Ok(()) => EXIT_OK,
            Err(error) => {
                let _ = writeln!(
                    error_output,
                    "viewer refused: {}",
                    observe::safe_text(&error.to_string())
                );
                EXIT_FAILED
            }
        };
    }
    let signals = match SignalCancellation::install() {
        Ok(signals) => signals,
        Err(error) => {
            let _ = writeln!(error_output, "installing SIGINT/SIGTERM handling: {error}");
            return EXIT_FAILED;
        }
    };
    let result = if options.command == PilotCommand::Resume {
        progress::run(error_output, |progress| {
            durable::resume(&options, signals.token(), progress)
        })
    } else if options.command == PilotCommand::Run {
        if options.durable {
            progress::run(error_output, |progress| {
                durable::run(&options, signals.token(), progress)
            })
        } else if options.authored_mission {
            authored_cycle::run(&options, signals.token())
        } else {
            cycle::run(&options, signals.token())
        }
    } else {
        progress::run(error_output, |progress| {
            run_review_with_progress(&options, signals.token(), progress)
        })
    };
    drop(signals);
    match result {
        Ok(summary) => {
            let _ = writeln!(
                output,
                "status={} result={}",
                summary.status,
                summary.result_path.display()
            );
            if summary.status == "paused" {
                EXIT_PAUSED
            } else if summary.completed {
                EXIT_OK
            } else {
                let label = if summary.status == "cancelled" {
                    "cancelled"
                } else {
                    "failed"
                };
                let _ = writeln!(error_output, "pilot {label}: {}", summary.reason);
                EXIT_FAILED
            }
        }
        Err(error) => {
            let _ = writeln!(error_output, "pilot refused: {error}");
            EXIT_FAILED
        }
    }
}

#[derive(Debug)]
pub struct PilotSummary {
    pub completed: bool,
    pub status: &'static str,
    pub reason: String,
    pub result_path: PathBuf,
}

/// Validates inputs, creates the fresh output directory, probes the runtime
/// version, and dispatches exactly one review attempt.
///
/// `Err` means nothing was dispatched. Once the output directory exists every
/// outcome, including a refused version probe, writes `pilot-result.json`.
pub fn run_review(
    options: &PilotOptions,
    cancellation: CancellationToken,
) -> Result<PilotSummary, PilotError> {
    run_review_with_progress(options, cancellation, progress::Progress::default())
}

fn run_review_with_progress(
    options: &PilotOptions,
    cancellation: CancellationToken,
    progress: progress::Progress,
) -> Result<PilotSummary, PilotError> {
    let feature_snapshot = feature_snapshot(options)?;
    let prompt = if matches!(options.command, PilotCommand::Code | PilotCommand::Run) {
        read_prompt_with_limit(&options.prompt_file, MAX_CODE_PROMPT_BYTES)?
    } else {
        read_prompt(&options.prompt_file)?
    };
    let route = routing::select(options, &prompt);
    let layout = create_output_layout(&options.output_dir, options.command)?;
    write_feature_snapshot(&layout.root, &feature_snapshot)?;
    let result = run_prepared(options, &prompt, &route, &layout, cancellation, progress);
    if let Err(error) = &result {
        if options.feature_requests.portal_output_cap()
            && !layout.root.join("portal-application.json").exists()
        {
            write_portal_application(
                &layout.root,
                &portal::unavailable("unavailable", &error.to_string()),
            )?;
        }
        write_result(
            &layout.root,
            &json!({
                "schema": RESULT_SCHEMA, "status": "failed",
                "reason": error.to_string(), "runtime": options.runtime,
                "model": route.model, "route": route.record(),
                "provider_dispatched": false,
            }),
        )?;
    }
    result
}

fn run_prepared(
    options: &PilotOptions,
    prompt: &str,
    route: &routing::Route,
    layout: &OutputLayout,
    cancellation: CancellationToken,
    progress: progress::Progress,
) -> Result<PilotSummary, PilotError> {
    write_artifact(&layout.root.join("prompt.md"), prompt.as_bytes())?;

    let supervisor =
        ProcessSupervisor::process_wide().map_err(|e| PilotError::Composition(e.to_string()))?;
    let snapshot = if matches!(options.command, PilotCommand::Code | PilotCommand::Run) {
        let repo = options.repo.as_deref().ok_or_else(|| {
            PilotError::Composition("code command lost its required repository".to_owned())
        })?;
        Some(
            snapshot::prepare(repo, &layout.root, &supervisor, &cancellation).map_err(
                |reason| PilotError::Repository {
                    path: repo.to_path_buf(),
                    reason,
                },
            )?,
        )
    } else {
        None
    };
    let worker = snapshot
        .as_ref()
        .map_or(layout.worker.as_path(), |snapshot| {
            snapshot.workspace.as_path()
        });
    let portal = if options.feature_requests.portal_output_cap() {
        Some(portal::Portal::prepare(&layout.root).map_err(PilotError::Composition)?)
    } else {
        None
    };
    let observed_version = match probe_runtime_version(options, worker, &cancellation, &supervisor)
    {
        Ok(version) => version,
        Err(error) => {
            let (status, observed) = match &error {
                PilotError::VersionMismatch { observed } => {
                    ("refused_version", Some(observed.clone()))
                }
                _ => ("refused_probe", None),
            };
            if portal.is_some() {
                write_portal_application(
                    &layout.root,
                    &portal::unavailable("not-dispatched", &error.to_string()),
                )?;
            }
            let record = json!({
                    "schema": RESULT_SCHEMA,
                    "status": status,
                    "reason": error.to_string(),
                    "runtime": options.runtime,
                    "runtime_executable_requested": options.executable().to_string_lossy(),
                    "runtime_version_required": options.required_version(),
                    "model": route.model,
            "route": route.record(),
                    "claude_code_version_required": if options.runtime == "claude" { Some(FIRST_USE_PILOT_CLAUDE_CODE_VERSION) } else { None },
                    "runtime_version_observed": observed,
                    "claude_code_version_observed": if options.runtime == "claude" { observed.clone() } else { None },
                    "review_dispatched": false,
                    "provider_dispatched": false,
                });
            let result_path = write_result(&layout.root, &record)?;
            return Ok(PilotSummary {
                completed: false,
                status,
                reason: error.to_string(),
                result_path,
            });
        }
    };

    let service_supervisor =
        ProcessSupervisor::process_wide().map_err(|e| PilotError::Composition(e.to_string()))?;
    let service = PilotProcessService::new(
        options.executable(),
        cancellation.clone(),
        service_supervisor,
        if options.runtime == "codex" {
            layout.shell_config.clone()
        } else {
            None
        },
    )?;
    let service = PilotProcessService {
        executable_id: options.runtime.clone(),
        progress,
        ..service
    };
    let outcome = dispatch_review(options, prompt, route, worker, &service)?;
    let observation = lock(&service.observation).take();
    let mut post_failure = None;
    let mut source_preserved = None;
    if let Some(snapshot) = &snapshot {
        match snapshot.diff(&supervisor) {
            Ok(diff) => {
                write_artifact(&layout.root.join("changes.diff"), &diff)?;
                if options.command == PilotCommand::Code
                    && options.runtime == "claude"
                    && diff.is_empty()
                {
                    post_failure = Some(
                        "Claude reported successful coding but the isolated snapshot did not change"
                            .to_owned(),
                    );
                }
            }
            Err(error) => post_failure = Some(error),
        }
        match snapshot.verify_source(&supervisor) {
            Ok(()) => source_preserved = Some(true),
            Err(error) => {
                source_preserved = Some(false);
                if post_failure.is_none() {
                    post_failure = Some(error);
                }
            }
        }
    }
    let portal_application = if let Some(portal) = portal {
        let validated = observation
            .as_ref()
            .ok_or_else(|| "provider observation is missing".to_owned())
            .and_then(|observation| {
                portal::require_complete_capture(&observation.summary)?;
                codex::parse_code_commands(&observation.stdout, worker).map_err(str::to_owned)
            })
            .and_then(|parsed| portal.validate(&parsed.commands));
        let report = match validated {
            Ok(report) => report,
            Err(reason) => {
                if post_failure.is_none() {
                    post_failure = Some(format!("Portal validation failed: {reason}"));
                }
                portal::unavailable("failed-validation", &reason)
            }
        };
        write_portal_application(&layout.root, &report)?;
        Some(report)
    } else {
        None
    };
    let coding = CodingFinish {
        portal_application,
        snapshot: snapshot.as_ref(),
        source_preserved,
        post_failure: post_failure.as_deref(),
    };
    finish(
        &layout.root,
        options,
        &observed_version,
        route,
        &outcome,
        observation,
        coding,
    )
}

fn read_prompt(path: &Path) -> Result<String, PilotError> {
    read_prompt_with_limit(path, MAX_PROMPT_BYTES)
}

fn read_prompt_with_limit(path: &Path, maximum: u64) -> Result<String, PilotError> {
    let fail = |reason: String| PilotError::Prompt {
        path: path.to_path_buf(),
        reason,
    };
    let metadata = fs::metadata(path).map_err(|e| fail(e.to_string()))?;
    if !metadata.is_file() {
        return Err(fail("not a regular file".to_owned()));
    }
    if metadata.len() > maximum {
        return Err(fail(format!(
            "{} bytes exceeds the {maximum}-byte pilot limit",
            metadata.len()
        )));
    }
    let prompt = fs::read_to_string(path).map_err(|e| fail(e.to_string()))?;
    if u64::try_from(prompt.len()).map_or(true, |len| len > maximum) {
        return Err(fail(
            "prompt grew beyond the pilot limit while reading".to_owned(),
        ));
    }
    if prompt.trim().is_empty() {
        return Err(fail("prompt is empty".to_owned()));
    }
    if prompt.contains('\0') {
        return Err(fail("prompt contains a NUL byte".to_owned()));
    }
    Ok(prompt)
}

struct OutputLayout {
    root: PathBuf,
    worker: PathBuf,
    shell_config: Option<PathBuf>,
}

/// Creates `<output>` (0700, must not exist) and an empty `<output>/worker`
/// used as the provider's working directory. Refuses any location inside a Git
/// checkout so no repository `CLAUDE.md` or settings sit above the worker.
fn create_output_layout(
    requested: &Path,
    command: PilotCommand,
) -> Result<OutputLayout, PilotError> {
    let root = resolve_output_root(requested)?;
    let fail = |reason: &str| PilotError::OutputDirectory {
        path: requested.to_path_buf(),
        reason: reason.to_owned(),
    };
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    builder.create(&root).map_err(|error| match error.kind() {
        io::ErrorKind::AlreadyExists => {
            fail("already exists; the pilot requires a fresh directory")
        }
        _ => PilotError::OutputDirectory {
            path: requested.to_path_buf(),
            reason: error.to_string(),
        },
    })?;
    let worker = root.join(if command == PilotCommand::Review {
        "worker"
    } else {
        "workspace"
    });
    if command == PilotCommand::Review {
        builder
            .create(&worker)
            .map_err(|error| PilotError::OutputDirectory {
                path: worker.clone(),
                reason: error.to_string(),
            })?;
    }
    let shell_config = if matches!(command, PilotCommand::Code | PilotCommand::Run) {
        let path = root.join("shell-config");
        builder
            .create(&path)
            .map_err(|error| PilotError::OutputDirectory {
                path: path.clone(),
                reason: error.to_string(),
            })?;
        Some(path)
    } else {
        None
    };
    Ok(OutputLayout {
        root,
        worker,
        shell_config,
    })
}

pub(crate) fn resolve_output_root(requested: &Path) -> Result<PathBuf, PilotError> {
    let fail = |reason: &str| PilotError::OutputDirectory {
        path: requested.to_path_buf(),
        reason: reason.to_owned(),
    };
    let name = match requested.components().next_back() {
        Some(Component::Normal(name)) => name.to_owned(),
        _ => return Err(fail("must end in a plain directory name")),
    };
    let parent = requested
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent).map_err(|_| fail("parent directory does not exist"))?;
    if let Some(checkout) = parent
        .ancestors()
        .find(|dir| dir.join(".git").symlink_metadata().is_ok())
    {
        return Err(PilotError::OutputDirectory {
            path: requested.to_path_buf(),
            reason: format!("is inside Git checkout {}", checkout.display()),
        });
    }
    Ok(parent.join(name))
}

fn probe_runtime_version(
    options: &PilotOptions,
    cwd: &Path,
    cancellation: &CancellationToken,
    supervisor: &ProcessSupervisor,
) -> Result<String, PilotError> {
    let mut spec = ProcessSpec::new(
        vec![options.executable(), OsString::from("--version")],
        VERSION_PROBE_TIMEOUT,
    )
    .map_err(|e| PilotError::VersionProbe(e.to_string()))?
    .with_max_output_bytes(VERSION_PROBE_MAX_OUTPUT)
    .with_cancellation(cancellation.clone());
    for key in INHERITED_ENVIRONMENT {
        spec = spec.with_inherited_env(key);
    }
    let report = supervisor
        .run(&spec, cwd)
        .map_err(|e| PilotError::VersionProbe(e.to_string()))?;
    if !report.is_success() {
        return Err(PilotError::VersionProbe(format!(
            "`--version` ended with {:?}",
            report.termination
        )));
    }
    let version_bytes = if options.runtime == "codex" {
        report.stdout.strip_prefix(b"codex-cli ").unwrap_or(b"")
    } else {
        &report.stdout
    };
    let observed = parse_claude_version(version_bytes)
        .ok_or_else(|| PilotError::VersionProbe("unrecognised `--version` output".to_owned()))?;
    if options.runtime == "codex"
        && std::str::from_utf8(version_bytes).map(str::trim).ok() != Some(observed.as_str())
    {
        return Err(PilotError::VersionProbe(
            "ambiguous Codex version output".to_owned(),
        ));
    }
    if observed != options.required_version() {
        return Err(PilotError::VersionMismatch { observed });
    }
    Ok(observed)
}

/// Extracts `X.Y.Z` from `X.Y.Z (Claude Code)`.
pub fn parse_claude_version(stdout: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(stdout).ok()?;
    let token = text.split_whitespace().next()?;
    let mut parts = token.split('.');
    let valid = (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    }) && parts.next().is_none();
    valid.then(|| token.to_owned())
}

fn dispatch_review(
    options: &PilotOptions,
    prompt: &str,
    route: &routing::Route,
    worker: &Path,
    service: &dyn ObservedProcessService,
) -> Result<AttemptOutcome, PilotError> {
    let phase = if options.command == PilotCommand::Code {
        "code"
    } else {
        "review"
    };
    dispatch_phase(options, prompt, route, worker, service, phase, phase)
}

fn dispatch_phase(
    options: &PilotOptions,
    prompt: &str,
    route: &routing::Route,
    worker: &Path,
    service: &dyn ObservedProcessService,
    phase: &str,
    role: &str,
) -> Result<AttemptOutcome, PilotError> {
    let composition = |error: &dyn std::fmt::Display| PilotError::Composition(error.to_string());
    let mut registry = ExecutorRegistry::new();
    if options.runtime == "codex" {
        let mode = if options.command == PilotCommand::Code {
            codex::Mode::Code
        } else {
            codex::Mode::Review
        };
        registry
            .register(
                "codex",
                Arc::new(
                    codex::CodexExecutor::new_with_process_binding(
                        mode,
                        service.executable_id(),
                        service.process_environment(),
                    )
                    .map_err(|e| composition(&e))?
                    .with_portal_directory(
                        if options.feature_requests.portal_output_cap() {
                            Some(
                                worker
                                    .parent()
                                    .ok_or_else(|| {
                                        PilotError::Composition(
                                            "workspace has no parent".to_owned(),
                                        )
                                    })?
                                    .join("portal-logs"),
                            )
                        } else {
                            None
                        },
                    ),
                ),
            )
            .map_err(|e| composition(&e))?;
    } else if options.command == PilotCommand::Code {
        let provider =
            ClaudeCodingExecutor::new(ClaudeProviderConfig::new()).map_err(|e| composition(&e))?;
        registry
            .register("claude", Arc::new(provider))
            .map_err(|e| composition(&e))?;
    } else {
        let provider = ClaudeProvider::new_first_use_pilot(ClaudeProviderConfig::new())
            .map_err(|e| composition(&e))?;
        registry
            .register("claude", Arc::new(provider))
            .map_err(|e| composition(&e))?;
    }
    let resolved = registry
        .resolve(&options.runtime)
        .map_err(|e| composition(&e))?;
    let request = phase_execution_request(options, prompt, route, worker, phase, role)?;
    let worker_id = format!("{phase}-worker-1");
    let identity =
        WorkerIdentity::new(MISSION_ID, phase, worker_id).map_err(|e| composition(&e))?;
    let mut sink = CountingEventSink::new().map_err(|e| composition(&e))?;
    let watchdog = StallWindow(options.timeout.min(MAX_STALL_WINDOW));
    let denied = DeniedEffects(
        EffectServiceError::new(
            EffectServiceErrorKind::Denied,
            "effects are disabled in the first-use pilot",
        )
        .map_err(|e| composition(&e))?,
    );
    let clock = MonotonicClock;
    let mut context = ExecutionContext::new(
        service,
        &clock,
        &watchdog,
        &denied,
        &mut sink,
        identity,
        clock.now() + options.timeout,
    );
    resolved
        .execute(&request, &mut context)
        .map_err(|e| composition(&e))
}

fn phase_execution_request(
    options: &PilotOptions,
    prompt: &str,
    route: &routing::Route,
    worker: &Path,
    phase: &str,
    role: &str,
) -> Result<ExecutionRequest, PilotError> {
    let composition = |error: &dyn std::fmt::Display| PilotError::Composition(error.to_string());
    let objective = if options.command == PilotCommand::Code {
        if options.feature_requests.portal_output_cap() {
            let prefix = CODE_OBJECTIVE_PREFIX.replace("Do not read or write outside the current workspace.", "Respect the workspace boundary with only the narrow Portal helper/log exception below.");
            format!(
                "{prefix}{}{prompt}",
                portal::policy(worker).map_err(PilotError::Composition)?
            )
        } else {
            format!("{CODE_OBJECTIVE_PREFIX}{prompt}")
        }
    } else {
        prompt.to_owned()
    };
    ExecutionRequest::new(ExecutionRequestDraft {
        mission: MISSION_ID.to_owned(),
        phase: phase.to_owned(),
        attempt: 1,
        revision: 1,
        objective,
        persona: route.persona.clone(),
        role: role.to_owned(),
        domain: "dev".to_owned(),
        skills: Vec::new(),
        dependencies: Vec::new(),
        expected_evidence: Vec::new(),
        constraints: vec![if options.command == PilotCommand::Code {
            "workspace-local coding without verification".to_owned()
        } else {
            "tool-less".to_owned()
        }],
        prior_context: String::new(),
        runtime: RuntimeFamily::parse(&options.runtime).map_err(|e| composition(&e))?,
        model: route.model.clone(),
        effort: route.effort,
        max_turns: if options.command == PilotCommand::Code && options.runtime == "claude" {
            FIRST_USE_PILOT_CLAUDE_MAX_TURNS
        } else {
            1
        },
        worker_dir: worker.to_path_buf(),
        target_dir: (options.command == PilotCommand::Code).then(|| worker.to_path_buf()),
        resume_from: None,
        hook_script: None,
    })
    .map_err(|error| composition(&error))
}

struct CodingFinish<'a> {
    portal_application: Option<Value>,
    snapshot: Option<&'a snapshot::RepositorySnapshot>,
    source_preserved: Option<bool>,
    post_failure: Option<&'a str>,
}

fn finish(
    root: &Path,
    options: &PilotOptions,
    observed_version: &str,
    route: &routing::Route,
    outcome: &AttemptOutcome,
    observation: Option<Observation>,
    coding: CodingFinish<'_>,
) -> Result<PilotSummary, PilotError> {
    let mut process = Value::Null;
    if let Some(observation) = &observation {
        write_artifact(
            &root.join(format!("{}-stdout.jsonl", options.runtime)),
            &observation.stdout,
        )?;
        write_artifact(
            &root.join(format!("{}-stderr.txt", options.runtime)),
            &observation.stderr,
        )?;
        process = observation.summary.clone();
    }
    let worker_usage = record_worker_usage(
        root,
        &options.runtime,
        observation.as_ref(),
        coding.portal_application.as_ref(),
    );
    let codex_protocol = observation
        .as_ref()
        .filter(|_| options.runtime == "codex")
        .and_then(|observation| {
            if options.command == PilotCommand::Code {
                codex::parse_code(&observation.stdout, root.join("workspace").as_path())
                    .ok()
                    .map(|parsed| {
                        json!({
                            "usage": parsed.usage,
                            "command_count": parsed.command_count,
                            "file_change_count": parsed.file_change_count,
                            "tool_failed": parsed.tool_failed,
                        })
                    })
            } else {
                codex::parse(&observation.stdout)
                    .ok()
                    .map(|parsed| json!({"warnings": parsed.warnings, "usage": parsed.usage}))
            }
        });
    let failures: Vec<Value> = outcome
        .failures()
        .iter()
        .map(|failure| json!({"kind": format!("{:?}", failure.kind()), "detail": failure.expose_detail()}))
        .collect();
    let cost = outcome.evidence().cost().map(|cost| {
        json!({
            "input_tokens": cost.input_tokens(),
            "output_tokens": cost.output_tokens(),
            "cache_creation_tokens": cost.cache_creation_tokens(),
            "total_cost_usd": cost.total_cost_usd(),
        })
    });
    let (completed, status, reason) = match (outcome, coding.post_failure) {
        (AttemptOutcome::Completed(_), None) => {
            let reason = if codex_protocol
                .as_ref()
                .is_some_and(|protocol| protocol["tool_failed"].as_bool() == Some(true))
            {
                "coding completed with failed command observations; independent verification pending"
                    .to_owned()
            } else {
                format!("{} reported a successful result", options.runtime)
            };
            (true, "completed", reason)
        }
        (AttemptOutcome::Completed(_), Some(reason)) => (false, "failed", reason.to_owned()),
        (AttemptOutcome::Incomplete(_), _) => {
            let termination = format!("{:?}", outcome.termination());
            let reason = failures
                .first()
                .and_then(|failure| failure["detail"].as_str())
                .map_or_else(
                    || termination.clone(),
                    |detail| format!("{termination}: {detail}"),
                );
            (false, "failed", reason)
        }
    };
    if let (true, Some(output)) = (completed, outcome.output()) {
        let name = if options.command == PilotCommand::Code {
            "answer.md"
        } else {
            "review.md"
        };
        write_artifact(&root.join(name), output.as_bytes())?;
    }
    let provider_completed = completed
        && (options.command != PilotCommand::Code
            || options.runtime != "codex"
            || codex_protocol.is_some());
    let record = json!({
        "schema": RESULT_SCHEMA,
        "status": status,
        "reason": reason,
        "review_dispatched": options.command == PilotCommand::Review,
        "provider_dispatched": true,
        "portal_application": coding.portal_application,
        "runtime": options.runtime,
                "runtime_executable_requested": options.executable().to_string_lossy(),
        "runtime_version_required": options.required_version(),
        "claude_code_version_required": if options.runtime == "claude" { Some(FIRST_USE_PILOT_CLAUDE_CODE_VERSION) } else { None },
        "runtime_version_observed": observed_version,
        "claude_code_version_observed": if options.runtime == "claude" { Some(observed_version) } else { None },
        "codex_protocol": codex_protocol,
        "worker_usage": worker_usage,
        "model": route.model,
        "route": route.record(),
        "timeout_secs": options.timeout.as_secs(),
        "termination": outcome.termination().map(|t| format!("{t:?}")),
        "failures": failures,
        "elapsed_ms": u64::try_from(outcome.elapsed().as_millis()).unwrap_or(u64::MAX),
        "cost": cost,
        "process": process,
        "source_repository": coding.snapshot.map(|snapshot| json!({
            "path": snapshot.source,
            "head": snapshot.head,
            "preserved": coding.source_preserved,
        })),
        "provider_completed": provider_completed,
        "tests_verified": if options.command == PilotCommand::Code { Some(false) } else { None },
    });
    let result_path = write_result(root, &record)?;
    Ok(PilotSummary {
        completed,
        status,
        reason,
        result_path,
    })
}

// Diagnostic persistence is best effort; a missing telemetry artifact must not
// relabel provider execution. The result always records unavailable evidence.
fn record_worker_usage(
    root: &Path,
    runtime: &str,
    observation: Option<&Observation>,
    portal_application: Option<&Value>,
) -> Value {
    record_worker_usage_scoped(root, runtime, observation, None, portal_application)
}

fn record_worker_usage_for_phase(
    root: &Path,
    runtime: &str,
    observation: Option<&Observation>,
    phase_id: &str,
) -> Value {
    record_worker_usage_scoped(root, runtime, observation, Some(phase_id), None)
}

fn record_worker_usage_scoped(
    root: &Path,
    runtime: &str,
    observation: Option<&Observation>,
    phase_id: Option<&str>,
    portal_application: Option<&Value>,
) -> Value {
    let mut captured = usage_runtime::capture(
        runtime,
        observation.map(|value| value.stdout.as_slice()),
        observation.and_then(|value| value.summary["stdout_discarded_bytes"].as_u64()),
    );
    if runtime != "claude" && runtime != "codex" {
        return captured;
    }
    if let Some(application) = portal_application {
        captured["portal_requested"] = json!("on");
        captured["portal_effective"] = if application["applied"] == true {
            json!("on")
        } else {
            Value::Null
        };
        captured["mode_source"] = json!("explicit-run-option; observed application report");
        captured["portal_application_status"] = application["status"].clone();
        captured["portal_application_artifact"] = json!("portal-application.json");
        if captured["report"].is_object() {
            captured["report"]["portal_mode"] = captured["portal_effective"].clone();
        }
    }
    if let Some(phase_id) = phase_id {
        captured["phase_id"] = json!(phase_id);
        if let Some(report) = captured.get_mut("report") {
            report["phase_id"] = json!(phase_id);
            if let Some(turns) = report.get_mut("turns").and_then(Value::as_array_mut) {
                for turn in turns {
                    turn["phase_id"] = json!(phase_id);
                }
            }
        }
        if let Some(events) = captured.get_mut("events").and_then(Value::as_array_mut) {
            for event in events {
                event["phase_id"] = json!(phase_id);
            }
        }
    }
    let mut summary = captured.clone();
    if let Some(object) = summary.as_object_mut() {
        object.remove("events");
        object.remove("report");
    }
    if captured["status"] == "available" {
        summary["summary"] = captured["report"]["summary"].clone();
        summary["quality"] = captured["report"]["quality"].clone();
        summary["assistant_message_count"] = captured["report"]["assistant_message_count"].clone();
        summary["tool_call_count"] = captured["report"]["tool_call_count"].clone();
        if runtime == "codex" {
            summary["provider_turn_count"] = captured["report"]["provider_turn_count"].clone();
            summary["granularity"] = json!("provider-turn");
        }
    }
    let saved = (|| -> Result<(), PilotError> {
        let report = serde_json::to_vec_pretty(&captured)
            .map_err(|error| PilotError::Composition(error.to_string()))?;
        write_artifact(&root.join("worker-usage.json"), &report)?;
        let mut bytes = Vec::new();
        if let Some(events) = captured["events"].as_array() {
            for event in events {
                let mut event = event.clone();
                event["schema"] = json!("nanika.worker-usage-event.v1");
                event["runtime"] = json!(runtime);
                event["implementation_revision"] = captured["implementation_revision"].clone();
                event["portal_requested"] = captured["portal_requested"].clone();
                event["portal_effective"] = captured["portal_effective"].clone();
                serde_json::to_writer(&mut bytes, &event)
                    .map_err(|error| PilotError::Composition(error.to_string()))?;
                bytes.push(b'\n');
            }
        }
        write_artifact(&root.join("worker-usage-events.jsonl"), &bytes)?;
        Ok(())
    })();
    if saved.is_ok() {
        summary["report_artifact"] = json!("worker-usage.json");
        summary["events_artifact"] = json!("worker-usage-events.jsonl");
    } else {
        summary["status"] = json!("unavailable");
        summary["reason"] = json!("telemetry artifact persistence failed");
        // Do not advertise an incomplete artifact set or its aggregates.
        if let Some(object) = summary.as_object_mut() {
            for key in [
                "summary",
                "quality",
                "assistant_message_count",
                "tool_call_count",
                "provider_turn_count",
                "granularity",
            ] {
                object.remove(key);
            }
        }
    }
    summary
}

fn feature_snapshot(options: &PilotOptions) -> Result<features::Snapshot, PilotError> {
    if options.feature_requests.portal_output_cap() && (options.durable || options.authored_mission)
    {
        return Err(PilotError::Usage(
            "Portal output capping is available only for standalone Codex code".to_owned(),
        ));
    }
    let command = match options.command {
        PilotCommand::Code => "code",
        PilotCommand::Review => "review",
        PilotCommand::Run => "run",
        _ => {
            return Err(PilotError::Usage(
                "feature settings require a fresh execution command".to_owned(),
            ));
        }
    };
    options
        .feature_requests
        .snapshot(&options.runtime, command)
        .map_err(PilotError::Usage)
}

fn write_portal_application(root: &Path, report: &Value) -> Result<(), PilotError> {
    let bytes =
        serde_json::to_vec_pretty(report).map_err(|e| PilotError::Composition(e.to_string()))?;
    write_artifact(&root.join("portal-application.json"), &bytes)
}

fn write_feature_snapshot(root: &Path, snapshot: &features::Snapshot) -> Result<(), PilotError> {
    let bytes = serde_json::to_vec_pretty(&snapshot.value())
        .map_err(|error| PilotError::Composition(error.to_string()))?;
    write_artifact(&root.join("run-features.json"), &bytes)
}

fn write_result(root: &Path, record: &Value) -> Result<PathBuf, PilotError> {
    let path = root.join("pilot-result.json");
    let mut bytes = serde_json::to_vec_pretty(record).map_err(|e| PilotError::Artifact {
        path: path.clone(),
        source: io::Error::other(e),
    })?;
    bytes.push(b'\n');
    write_artifact(&path, &bytes)?;
    Ok(path)
}

fn write_artifact(path: &Path, bytes: &[u8]) -> Result<(), PilotError> {
    let artifact = |source| PilotError::Artifact {
        path: path.to_path_buf(),
        source,
    };
    let parent = path
        .parent()
        .ok_or_else(|| artifact(io::Error::other("artifact has no parent")))?;
    require_real_directory_chain(parent).map_err(artifact)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
        .map_err(artifact)?;
    file.write_all(bytes).map_err(artifact)?;
    file.sync_all().map_err(artifact)?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(artifact)
}

fn require_real_directory_chain(path: &Path) -> io::Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::RootDir) {
            continue;
        }
        let metadata = current.symlink_metadata()?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "artifact parent contains a non-directory or symbolic link",
            ));
        }
    }
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Observation {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    summary: Value,
}

trait ObservedProcessService: ProcessService {
    fn executable_id(&self) -> &str;
    fn process_environment(&self) -> Vec<(String, String)>;
    fn take_observation(&self) -> Option<Observation>;
}

/// Pilot-local ambient adapter from the exec `ProcessService` contract to the
/// process supervisor. It admits only the selected executable id, forwards the
/// provider's explicit environment, and adds [`INHERITED_ENVIRONMENT`].
pub struct PilotProcessService {
    executable: OsString,
    executable_id: String,
    cancellation: CancellationToken,
    supervisor: ProcessSupervisor,
    shell_config: Option<PathBuf>,
    observation: Mutex<Option<Observation>>,
    progress: progress::Progress,
    errors: ServiceErrors,
}

/// Errors are validated once at composition so `execute` never has to
/// construct (and possibly fail to construct) one mid-run.
struct ServiceErrors {
    preflight: ProcessServiceError,
    not_enrolled: ProcessServiceError,
    stdin: ProcessServiceError,
    bounds: ProcessServiceError,
    spawn: ProcessServiceError,
    receipt: ProcessServiceError,
}

impl PilotProcessService {
    fn new(
        executable: OsString,
        cancellation: CancellationToken,
        supervisor: ProcessSupervisor,
        shell_config: Option<PathBuf>,
    ) -> Result<Self, PilotError> {
        let error = |kind, detail: &str| {
            ProcessServiceError::new(kind, detail)
                .map_err(|e| PilotError::Composition(e.to_string()))
        };
        Ok(Self {
            executable,
            executable_id: "claude".to_owned(),
            cancellation,
            supervisor,
            shell_config,
            observation: Mutex::new(None),
            progress: progress::Progress::default(),
            errors: ServiceErrors {
                preflight: error(
                    ProcessServiceErrorKind::InvalidRequest,
                    "preflight proof did not bind to the pilot service",
                )?,
                not_enrolled: error(
                    ProcessServiceErrorKind::NotEnrolled,
                    "the pilot admits only the selected executable",
                )?,
                stdin: error(
                    ProcessServiceErrorKind::InvalidRequest,
                    "the pilot does not supply stdin",
                )?,
                bounds: error(
                    ProcessServiceErrorKind::InvalidRequest,
                    "provider request violates the supervisor bounds",
                )?,
                spawn: error(
                    ProcessServiceErrorKind::Spawn,
                    "provider could not be spawned",
                )?,
                receipt: error(
                    ProcessServiceErrorKind::OutcomeIndeterminate,
                    "supervisor report violated the process receipt contract",
                )?,
            },
        })
    }
}

impl Cancellation for PilotProcessService {
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

impl ProcessService for PilotProcessService {
    fn finish_preflight(
        &self,
        request: &ProcessRequest,
        preflight: ProcessPreflight<'_>,
    ) -> Result<(), ProcessServiceError> {
        preflight
            .bind(self, request)
            .map(|_| ())
            .ok_or_else(|| self.errors.preflight.clone())
    }

    fn execute(
        &self,
        request: &ProcessRequest,
        budget: ProcessBudget,
    ) -> Result<ProcessReceipt, ProcessServiceError> {
        if request.executable_id() != self.executable_id {
            return Err(self.errors.not_enrolled.clone());
        }
        let stdin = request.expose_stdin();
        if stdin.is_some() && self.executable_id != "codex" {
            return Err(self.errors.stdin.clone());
        }
        let mut argv = Vec::with_capacity(request.expose_arguments().len() + 1);
        argv.push(self.executable.clone());
        argv.extend(request.expose_arguments().iter().map(OsString::from));
        let mut spec = ProcessSpec::new(argv, budget.remaining())
            .map_err(|_| self.errors.bounds.clone())?
            .with_hard_deadline_at(budget.hard_deadline())
            .with_stall_timeout(budget.stall_window())
            .with_max_output_bytes(request.max_output_bytes())
            .with_cancellation(self.cancellation.clone());
        for (key, value) in request.expose_environment() {
            spec = spec.with_env(key, value);
        }
        for key in INHERITED_ENVIRONMENT {
            spec = spec.with_inherited_env(key);
        }
        if let Some(stdin) = stdin {
            spec = spec.with_stdin(stdin.to_vec());
        }
        if let Some(shell_config) = &self.shell_config {
            spec = spec
                .with_env("ZDOTDIR", shell_config)
                .with_env("BASH_ENV", "/dev/null")
                .with_env("ENV", "/dev/null");
        }
        let report = if self.progress.enabled() {
            self.progress
                .forward_observed("standalone", Some(self.executable_id.as_str()), |output| {
                    self.supervisor
                        .run(&spec.with_output_sender(output), request.working_root())
                })
                .map_err(|_| self.errors.spawn.clone())?
        } else {
            self.supervisor.run(&spec, request.working_root())
        }
        .map_err(|_| self.errors.spawn.clone())?;
        let receipt = receipt_from_report(&report).map_err(|_| self.errors.receipt.clone())?;
        *lock(&self.observation) = Some(Observation {
            summary: report_summary(&report, request),
            stdout: report.stdout,
            stderr: report.stderr,
        });
        Ok(receipt)
    }
}

impl ObservedProcessService for PilotProcessService {
    fn executable_id(&self) -> &str {
        &self.executable_id
    }

    fn process_environment(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    fn take_observation(&self) -> Option<Observation> {
        lock(&self.observation).take()
    }
}

fn receipt_from_report(
    report: &ProcessReport,
) -> Result<ProcessReceipt, orchestrator_exec::ServiceContractError> {
    let termination = match report.termination {
        ProcessTermination::Exited(code) => {
            ProcessTerminationReceipt::Exited(ProcessExitStatus::code(code)?)
        }
        ProcessTermination::Signaled(signal) => {
            ProcessTerminationReceipt::Exited(ProcessExitStatus::signal(signal)?)
        }
        ProcessTermination::Timeout => ProcessTerminationReceipt::DeadlineExceeded,
        ProcessTermination::Stalled => ProcessTerminationReceipt::Stalled,
        ProcessTermination::Cancelled => ProcessTerminationReceipt::Cancelled,
        ProcessTermination::OutputLimit => ProcessTerminationReceipt::OutputLimit,
        ProcessTermination::InfrastructureError => ProcessTerminationReceipt::SupervisorFailure,
        ProcessTermination::UnresolvedOwnership => ProcessTerminationReceipt::UnresolvedOwnership,
    };
    ProcessReceipt::new(
        termination,
        report.stdout.clone(),
        report.stderr.clone(),
        report.stdout_discarded_bytes,
        report.stderr_discarded_bytes,
        report.cleanup_complete && report.infrastructure_failures.is_empty(),
        report.elapsed,
    )
}

/// Records argv flags with the Claude prompt value (the argument after `-p`)
/// removed; prompts are saved verbatim as `prompt.md`.
fn report_summary(report: &ProcessReport, request: &ProcessRequest) -> Value {
    let mut argv = Vec::new();
    let mut redact_next = false;
    for argument in request.expose_arguments() {
        if redact_next {
            argv.push("<prompt.md>".to_owned());
            redact_next = false;
        } else {
            redact_next = argument == "-p";
            argv.push(argument.clone());
        }
    }
    json!({
        "argv_after_executable": argv,
        "working_root": request.working_root(),
        "termination": format!("{:?}", report.termination),
        "spawned": report.spawned,
        "cancellation_observed": report.cancellation_observed,
        "deadline_observed": report.deadline_observed,
        "stall_observed": report.stall_observed,
        "term_sent": report.term_sent,
        "kill_sent": report.kill_sent,
        "cleanup_complete": report.cleanup_complete,
        "infrastructure_failures": format!("{:?}", report.infrastructure_failures),
        "stdout_bytes": report.stdout.len(),
        "stderr_bytes": report.stderr.len(),
        "stdout_discarded_bytes": report.stdout_discarded_bytes,
        "stderr_discarded_bytes": report.stderr_discarded_bytes,
        "elapsed_ms": u64::try_from(report.elapsed.as_millis()).unwrap_or(u64::MAX),
    })
}

struct MonotonicClock;

impl Clock for MonotonicClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// The supervisor enforces the same window through `with_stall_timeout`.
struct StallWindow(Duration);

impl WatchdogPolicy for StallWindow {
    fn evaluate(&self, now: Instant, last_activity: Instant) -> WatchdogDecision {
        if now.saturating_duration_since(last_activity) >= self.0 {
            WatchdogDecision::Stalled
        } else {
            WatchdogDecision::Continue {
                next_check: last_activity + self.0,
            }
        }
    }

    fn stall_window(&self) -> Duration {
        self.0
    }
}

struct DeniedEffects(EffectServiceError);

impl EffectService for DeniedEffects {
    fn execute(
        &self,
        _request: &EffectRequest,
        _budget: EffectBudget<'_>,
    ) -> Result<EffectReceipt, EffectServiceError> {
        Err(self.0.clone())
    }
}

/// In-memory sink: the pilot keeps no durable event log.
struct CountingEventSink {
    emitted: i64,
    unavailable: EventSinkError,
}

impl CountingEventSink {
    fn new() -> Result<Self, orchestrator_exec::WorkerEventError> {
        Ok(Self {
            emitted: 0,
            unavailable: EventSinkError::new(
                EventSinkErrorKind::Unavailable,
                "pilot event receipt could not be allocated",
            )?,
        })
    }
}

impl EventSink for CountingEventSink {
    fn emit(&mut self, _event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError> {
        let sequence = self
            .emitted
            .checked_add(1)
            .ok_or_else(|| self.unavailable.clone())?;
        let receipt = EventReceipt::new(
            format!("evt_first_use_pilot_{sequence:016x}"),
            rfc3339_now(),
            sequence,
        )
        .map_err(|_| self.unavailable.clone())?;
        self.emitted = sequence;
        Ok(receipt)
    }
}

fn rfc3339_now() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let seconds = elapsed.as_secs();
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let rem = seconds % 86_400;
    // Howard Hinnant's days-from-civil inverse.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:09}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
        elapsed.subsec_nanos()
    )
}

/// SIGINT/SIGTERM cancel the shared token; the supervisor then terminates and
/// reaps the provider process group. Dropping closes the handle and joins.
struct SignalCancellation {
    token: CancellationToken,
    handle: SignalHandle,
    thread: Option<JoinHandle<()>>,
}

impl SignalCancellation {
    fn install() -> io::Result<Self> {
        let mut signals = Signals::new([SIGINT, SIGTERM])?;
        let handle = signals.handle();
        let token = CancellationToken::new();
        let signal_token = token.clone();
        let thread = thread::Builder::new()
            .name("first-use-pilot-signals".into())
            .spawn(move || {
                for _ in signals.forever() {
                    signal_token.cancel();
                }
            });
        match thread {
            Ok(thread) => Ok(Self {
                token,
                handle,
                thread: Some(thread),
            }),
            Err(error) => {
                handle.close();
                Err(error)
            }
        }
    }

    fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Drop for SignalCancellation {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod authored_cycle_tests;

#[cfg(test)]
mod tests;

/// Replays bounded saved Claude usage into a fresh observational report.
/// Does not change provider execution status or canonical accounting.
pub fn replay_usage(input: &Path, output: &Path) -> Result<(), String> {
    usage::execute(input, output)
}
