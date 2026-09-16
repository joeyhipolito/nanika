//! Native CLI boundary for Go-compatible root/run parsing and read-only composition.

use orchestrator_app::{ReadOnlyEventLogAuthority, ReadOnlyEventLogTarget};
use std::{
    io::{self, Write},
    path::Path,
};
use thiserror::Error;

mod cmd;
mod composition;
mod isolated_home;
pub mod render;
mod run;

#[cfg(all(unix, feature = "verification-process-canary"))]
pub use cmd::run_hermetic_canary_worker;
pub use composition::{
    CompositionError, ExecutionEnrollment, FixtureEnrollment, LIVE_PROVIDER_RUNTIME,
    SealedPhaseOutcome, SealedPhaseRun, SealedRun, SealedRunError, SealedRunReport,
    UnenrolledReason, governed_git_run, seal, worker_effect_service,
};
#[cfg(any(test, feature = "test-support"))]
pub use composition::{FixtureEvidenceEnrollment, FixtureGitEnrollment};
pub use run::{
    GitRunError, GitRunPlan, GitRunReceipts, GovernedGit, PersistentFlags, ResolvedPhase,
    ResolvedRun, RunExecutionError, RunFlags, RunResolutionContext, resolve_with_context,
    run_one_phase, run_one_phase_with_governed_git,
};

const HELP: &str = r#"Orchestrator decomposes tasks into phases, spawns specialized workers,
and coordinates their execution. Each worker gets a persona, skills,
and context — then runs as a Claude session in its own directory.

Folders are workers. CLAUDE.md is personality. Execution is just
running Claude CLI in a directory.

Usage:
  orchestrator [command]

Available Commands:
  advisor             Manage operator-intended GLM advisor jobs
  archive             Archive dead-weight learnings (dry-run by default)
  audit               Audit tools for mission quality tracking
  backfill-embeddings Fill missing embeddings on existing learnings (dry-run by default)
  barok               Barok output-compression debug and status
  cancel              Cancel a running mission
  cleanup             Remove old workspaces (older than 7 days)
  compare             Compare v1 vs v2 orchestrator performance metrics
  completion          Generate the autocompletion script for the specified shell
  daemon              Manage the event relay daemon
  discipline          Reasoning-discipline layer debug and status
  doctor              Run environment health checks
  dream               Mine agent session transcripts for durable learnings
  events              Inspect mission event logs
  evidence            Manage the local interview-evidence ledger
  help                Help about any command
  hooks               Hook commands for context management during worker sessions
  ingest              Ingest external content into the learnings database
  memory              Manage persona memory entries
  metrics             Show mission execution history
  prune               Prune old and low-quality learnings from the database
  routing             Manage routing memory (target profiles and patterns)
  run                 Execute a task or mission
  stats               Show learning database statistics
  status              Show recent workspace status
  sync                Show and reconcile issue/workspace/audit links
  templates           Manage mission templates

Flags:
      --domain string         task domain (dev/personal/work/creative/academic) (default "dev")
      --dry-run               show plan without executing
  -h, --help                  help for orchestrator
      --max-turns int         max agentic turns per worker (0 = use persona-aware default)
      --model string          force model for all workers
      --nanika-dir string     path to nanika directory (default: ~/nanika)
      --personas-dir string   path to personas directory (default: ~/nanika/personas/)
      --sequential            force sequential execution
  -v, --verbose               verbose output

Use "orchestrator [command] --help" for more information about a command.
"#;

const RUN_HELP: &str = r#"Run a task description or a mission file (.md).
Simple tasks get a single worker. Complex tasks are decomposed
into phases with specialized workers.

Use --template to run a saved frozen plan:
  orchestrator run --template <name> [key=value ...]
Use --save-template to freeze a plan after execution:
  orchestrator run <task> --save-template <name>

Usage:
  orchestrator run [task or mission-file] [flags]

Flags:
      --codex-review            post @codex review request comment on the PR after creation (requires --pr)
      --gate-mode string        quality gate mode: block (fail phase on bad output) or warn (log and continue) (default "block")
      --git-isolate             execute in an isolated git worktree (auto-enabled for git-repo targets) (default true)
  -h, --help                    help for run
      --no-comment              skip posting summary comment to Linear issue after completion
      --no-draft                create PR as ready-for-review instead of draft (only used with --pr)
      --no-git                  skip git isolation even when target is a git repository
      --no-learnings            skip learning retrieval and injection
      --no-persistent-worker    disable persistent worker assignment for all phases
      --no-review               skip automatic review-phase injection after decomposition
      --pr                      open a GitHub pull request after a successful run (requires gh CLI)
      --resume string           resume from workspace path
      --review-runtime string   runtime for auto-injected review phases (claude, codex, both); default uses policy
      --runtime string          override runtime for all policy-applied phases (claude, anthropic-api, openai-api, openrouter, gemini-api); env NANIKA_DEFAULT_RUNTIME is also honoured
      --save-template string    save plan as reusable template after execution
      --stall-timeout string    watchdog stall timeout per phase (e.g. 10m, 30m); overrides ORCHESTRATOR_STALL_TIMEOUT env var
      --template string         run from a saved template (skips decomposition)

Global Flags:
      --domain string         task domain (dev/personal/work/creative/academic) (default "dev")
      --dry-run               show plan without executing
      --max-turns int         max agentic turns per worker (0 = use persona-aware default)
      --model string          force model for all workers
      --nanika-dir string     path to nanika directory (default: ~/nanika)
      --personas-dir string   path to personas directory (default: ~/nanika/personas/)
      --sequential            force sequential execution
  -v, --verbose               verbose output
"#;

/// Go's `statusCmd` help block, byte-for-byte.
///
/// `status` reaches [`write_usage`] rather than the root [`HELP`] when it
/// refuses a flag, so the refused invocation prints the command's own usage
/// exactly as cobra does — the root block names a different command line and
/// would not be parity.
const STATUS_HELP: &str = r#"Show recent workspace status

Usage:
  orchestrator status [flags]

Flags:
  -h, --help   help for status

Global Flags:
      --domain string         task domain (dev/personal/work/creative/academic) (default "dev")
      --dry-run               show plan without executing
      --max-turns int         max agentic turns per worker (0 = use persona-aware default)
      --model string          force model for all workers
      --nanika-dir string     path to nanika directory (default: ~/nanika)
      --personas-dir string   path to personas directory (default: ~/nanika/personas/)
      --sequential            force sequential execution
  -v, --verbose               verbose output
"#;

/// Structured CLI boundary failures.
#[derive(Debug, Error)]
pub enum CliError {
    /// Writing command output failed.
    #[error("cannot write command output: {0}")]
    Output(#[from] io::Error),
    /// `orchestrator run` argument resolution failed.
    #[error(transparent)]
    RunResolve(#[from] run::RunResolveError),
    /// A persistent flag failed before a concrete command was selected.
    #[error(transparent)]
    RootFlag(run::RunResolveError),
    /// Read-only system composition failed.
    #[error(transparent)]
    Composition(#[from] CompositionError),
    /// Publicly typed daemon service failure. Callers can distinguish retry,
    /// cursor, enrollment, identity, and resource-budget failures without
    /// parsing display text.
    #[error(transparent)]
    Daemon(#[from] orchestrator_daemon::DaemonError),
    /// No enrolled provider composition exists for this run.
    ///
    /// The task and resolved paths are intentionally not retained in the error.
    /// B5-DESIGN §2.2: this is produced from the enrollment branch in
    /// [`run_system_with_enrollment`] and nowhere else in `src/`.
    #[error("Rust execution is not enrolled; use --dry-run or the Go rollback command")]
    ExecutionNotEnrolled,
    /// The sealed composition root failed while driving an enrolled run.
    ///
    /// Carries the formatted message only, matching this file's existing
    /// pattern of not exposing internal error structure on `CliError`.
    #[error("{0}")]
    SealedRun(String),
    /// The root command name is not present in the accepted Go command tree.
    #[error("unknown command {0:?} for \"orchestrator\"")]
    UnknownCommand(String),
    /// The architecture-foundation CLI does not implement the requested command yet.
    ///
    /// Arguments are intentionally not retained because they may contain credentials.
    #[error("unsupported architecture-foundation command")]
    UnsupportedCommand,
    /// `status`/`events`/`metrics` argument parsing or execution failed.
    /// Carries the formatted message only — `cmd::CmdError` stays
    /// crate-private (matching this file's existing pattern of not exposing
    /// internal command-error structure on the public `CliError` surface).
    #[error("{0}")]
    Cmd(String),
    /// A home-backed inspection command (`status`, `events`, or `audit`) was dispatched
    /// through [`run`], which deliberately does not resolve a runtime home.
    /// Only [`run_system`] can serve these commands.
    #[error("{command} requires runtime-home resolution; use the run_system entry point")]
    RequiresRuntimeHome { command: &'static str },
    /// Metrics was dispatched through [`run`] without an injected
    /// owner-mediated query service.
    #[error("metrics requires an owner-mediated query service; use the run_system entry point")]
    RequiresMetricsQueryService,
    /// A learning-maintenance command was dispatched without an injected
    /// read-only store adapter.
    ///
    /// B3-DESIGN §3.1 constructs the Go learning adapter from an
    /// `IsolatedFixtureRoot`, never from a path, so there is no production
    /// composition that can serve these commands yet — the production reader
    /// arrives with B5's fence-and-grant step. Use
    /// [`run_learning_with_reader`] to serve them under fixture authority.
    #[error(
        "{command} requires a read-only learning-store adapter, which is only constructible \
         under fixture authority in this slice; use the run_learning_with_reader entry point"
    )]
    RequiresLearningReader {
        /// The Go command name that was dispatched.
        command: &'static str,
    },
    /// An injected entry point was handed arguments naming a different command.
    #[error("expected an {expected} invocation")]
    NotThisCommand {
        /// The command family the entry point serves.
        expected: &'static str,
    },
}

impl From<cmd::CmdError> for CliError {
    fn from(error: cmd::CmdError) -> Self {
        match error {
            cmd::CmdError::Daemon(cmd::DaemonCommandError::Service(source)) => Self::Daemon(source),
            cmd::CmdError::Daemon(cmd::DaemonCommandError::Output(source)) => Self::Output(source),
            other => Self::Cmd(other.to_string()),
        }
    }
}

/// Runs the native system composition root. Run commands load read-only home,
/// routing, persona, and environment inputs; help commands do not inspect
/// runtime state. On command errors, this writes a usage block to
/// `error_output` when the accepted Go CLI would do so; the binary prints the
/// returned error once.
pub fn run_system<I, S>(
    arguments: I,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    run_system_with_enrollment(arguments, output, error_output, None)
}

/// The same entry point, with B5-DESIGN §1.1's enrollment parameter exposed.
///
/// This is the only `composition::seal` call site in `src/`, and
/// [`run_system`] is the only caller that supplies a value for `enrollment` —
/// `None`. A gate reaches the enrolled half by passing `Some(&enrollment)`
/// through the *same* argv path a user types; it does not get a second
/// entry point, a flag, or an environment variable.
///
/// In a build without `test-support`, `FixtureEnrollment` is uninhabited, so
/// `enrollment` can only ever be `None` here.
///
/// # Errors
/// Returns [`CliError`] exactly as [`run_system`] does.
pub fn run_system_with_enrollment<I, S>(
    arguments: I,
    output: &mut impl Write,
    error_output: &mut impl Write,
    enrollment: Option<&FixtureEnrollment<'_>>,
) -> Result<(), CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let arguments: Vec<String> = arguments.into_iter().map(Into::into).collect();
    let mut usage = HELP;
    let command = match split_root_command_with_usage(&arguments, &mut usage) {
        Ok(command) => command,
        Err(error) => {
            if matches!(error, CliError::RootFlag(_)) {
                write_usage(usage, error_output)?;
            }
            return Err(error);
        }
    };
    match command {
        RootCommand::RootHelp => output.write_all(HELP.as_bytes())?,
        RootCommand::Run(arguments) => {
            let (flags, positional) = match run::parse(&arguments) {
                Ok(parsed) => parsed,
                Err(error) => {
                    write_usage(RUN_HELP, error_output)?;
                    return Err(error.into());
                }
            };
            if flags.help {
                output.write_all(RUN_HELP.as_bytes())?;
                return Ok(());
            }
            if let Err(error) = run::validate_invocation(&flags, &positional) {
                write_usage(RUN_HELP, error_output)?;
                return Err(error.into());
            }
            let result = (|| {
                let mut sealed = composition::seal(&flags, enrollment)?;
                for warning in sealed.warnings() {
                    writeln!(error_output, "{warning}")?;
                }
                let resolved = run::resolve_with_context(&arguments, sealed.resolution())?;
                // Dry-run and offline consume exactly seal 1 and stop: offline
                // renders the deterministic keyword-fallback plan compiled
                // during resolution, and dispatching a released phase is the
                // `run::run_one_phase` seam. Only the ordinary run needs an
                // enrolled executor, and B5-DESIGN §2.1 resolves that over the
                // sealed home state and the sealed registry rather than over a
                // flag.
                let mut executed = false;
                if !resolved.flags.persistent.dry_run && !resolved.flags.offline {
                    // `ExecutionNotEnrolled` is produced here and nowhere else
                    // in `src/`. With `run_system`'s `None` the reason is
                    // always `NoFixtureEnrollment`, because seals 2-13 do not
                    // exist; with an enrollment it is `NoExecutorForRuntime`,
                    // i.e. `ExecutorRegistry::resolve` finding nothing for a
                    // live-provider family that has no registration site.
                    if !sealed.enrollment_for_plan(&resolved).is_enrolled() {
                        return Err(CliError::ExecutionNotEnrolled);
                    }
                    execute_sealed_run(&mut sealed, &resolved)?;
                    executed = true;
                }
                run::render(&resolved, executed, output)?;
                Ok(())
            })();
            if let Err(error) = result {
                if !matches!(error, CliError::Output(_)) {
                    write_usage(RUN_HELP, error_output)?;
                }
                return Err(error);
            }
        }
        RootCommand::Status { help: true } => output.write_all(STATUS_HELP.as_bytes())?,
        RootCommand::Status { help: false } => {
            let home = composition::resolve_home_path()?;
            cmd::run_status(&home, output, error_output).map_err(cmd::CmdError::from)?;
        }
        RootCommand::Events(command) => {
            let home = composition::resolve_home_path()?;
            match command {
                cmd::EventsCommand::List => {
                    cmd::run_events_list(&home, output).map_err(cmd::CmdError::from)?;
                }
                cmd::EventsCommand::Replay {
                    mission_id,
                    raw_json,
                } => {
                    cmd::run_events_replay(&home, &mission_id, raw_json, output)
                        .map_err(cmd::CmdError::from)?;
                }
                cmd::EventsCommand::Tail {
                    mission_id,
                    raw_json,
                } => {
                    cmd::run_events_tail(&home, &mission_id, raw_json, output, error_output)
                        .map_err(cmd::CmdError::from)?;
                }
            }
        }
        RootCommand::Daemon(command) => {
            if cmd::daemon_is_help(&command) {
                cmd::run_daemon(command, Path::new(""), output).map_err(cmd::CmdError::from)?;
                return Ok(());
            }
            let home = composition::resolve_home_path()?;
            cmd::run_daemon(command, &home, output).map_err(cmd::CmdError::from)?;
        }
        RootCommand::Metrics(command) => {
            let service = composition::metrics_query_service();
            cmd::run_metrics(&service, &command, output).map_err(cmd::CmdError::from)?;
        }
        RootCommand::Learning(command) => {
            // Risk 4: no production learning authority is composed in B3.
            return Err(CliError::RequiresLearningReader {
                command: command.name(),
            });
        }
        RootCommand::AuditScorecard(flags) => {
            let home = composition::resolve_home_path()?;
            let target = bind_audit_target(&home)?;
            cmd::run_audit_scorecard(target.as_ref(), &flags, output)
                .map_err(cmd::CmdError::from)?;
        }
        RootCommand::BarokStatus => cmd::run_barok_status(output)?,
        RootCommand::DisciplineStatus => cmd::run_discipline_status(output)?,
        #[cfg(all(unix, feature = "verification-process-canary"))]
        RootCommand::HermeticCanary => {
            cmd::run_hermetic_canary(output).map_err(cmd::CmdError::from)?;
        }
    }
    Ok(())
}

/// Drives an enrolled sealed run to terminal.
///
/// No longer split by profile: B5-DESIGN §7's isolated-home door seals an
/// executor in a build with no `test-support`, so there is a run to drive in
/// every profile. A run that reached here is enrolled — the caller has already
/// returned `ExecutionNotEnrolled` otherwise.
fn execute_sealed_run(
    sealed: &mut SealedRun<'_>,
    resolved: &run::ResolvedRun,
) -> Result<(), CliError> {
    sealed
        .execute(resolved)
        .map_err(|error| CliError::SealedRun(error.to_string()))?;
    Ok(())
}

/// Runs a `metrics` invocation against a caller-supplied query service.
///
/// The full argument vector is parsed, so root-flag placement, the `--flag=value`
/// inline form, and subcommand selection are exercised exactly as the binary
/// exercises them — only the storage owner is injected.
///
/// This is the seam `cmd::metrics` was written against: it takes an
/// already-typed [`orchestrator_app::MetricsQueryService`] and never a path, so
/// it grants no new storage authority. B3-DESIGN Risk 4 keeps the production
/// composition on `UnenrolledMetricsQueryService` until B5 fences the Go
/// writer; this entry point is how a fixture-authority caller renders metrics
/// in the meantime.
///
/// # Errors
/// Returns [`CliError::NotThisCommand`] when `arguments` do not name a
/// `metrics` invocation, plus any parse or query failure.
pub fn run_metrics_with_service<I, S, Q>(
    arguments: I,
    service: &Q,
    output: &mut impl Write,
) -> Result<(), CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    Q: orchestrator_app::MetricsQueryService + ?Sized,
{
    let arguments: Vec<String> = arguments.into_iter().map(Into::into).collect();
    match split_root_command(&arguments)? {
        RootCommand::Metrics(command) => {
            cmd::run_metrics(service, &command, output).map_err(cmd::CmdError::from)?;
            Ok(())
        }
        _ => Err(CliError::NotThisCommand {
            expected: "metrics",
        }),
    }
}

/// Runs a `stats`/`prune`/`archive`/`backfill-embeddings` invocation against a
/// caller-supplied read-only learning-store adapter.
///
/// The reader is [`orchestrator_app::GoLearningReader`], which carries no
/// update, delete, or DDL method, so this entry point cannot express a
/// mutation regardless of the flags it is handed.
///
/// # Errors
/// Returns [`CliError::NotThisCommand`] when `arguments` do not name one of the
/// four learning commands, plus any parse or store failure.
pub fn run_learning_with_reader<I, S, R>(
    arguments: I,
    reader: &R,
    output: &mut impl Write,
) -> Result<(), CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    R: orchestrator_app::GoLearningReader + ?Sized,
{
    let arguments: Vec<String> = arguments.into_iter().map(Into::into).collect();
    match split_root_command(&arguments)? {
        RootCommand::Learning(command) => {
            cmd::run_learning(reader, &command, output).map_err(cmd::CmdError::from)?;
            Ok(())
        }
        _ => Err(CliError::NotThisCommand {
            expected: "learning-maintenance",
        }),
    }
}

/// Runs the CLI against injected output without reading runtime-home state.
pub fn run<I, S>(arguments: I, output: &mut impl Write) -> Result<(), CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let arguments: Vec<String> = arguments.into_iter().map(Into::into).collect();
    match split_root_command(&arguments)? {
        RootCommand::RootHelp => output.write_all(HELP.as_bytes())?,
        RootCommand::Run(arguments) => {
            let (flags, _) = run::parse(&arguments)?;
            if flags.help {
                output.write_all(RUN_HELP.as_bytes())?;
                return Ok(());
            }
            let resolved = run::resolve(&arguments)?;
            // This entry point resolves without a composition root, so it can
            // never have executed anything.
            run::render(&resolved, false, output)?;
        }
        RootCommand::Status { help: true } => output.write_all(STATUS_HELP.as_bytes())?,
        RootCommand::Status { help: false } => {
            return Err(CliError::RequiresRuntimeHome { command: "status" });
        }
        RootCommand::Events(_) => {
            return Err(CliError::RequiresRuntimeHome { command: "events" });
        }
        RootCommand::Daemon(command) => {
            if cmd::daemon_is_help(&command) {
                cmd::run_daemon(command, Path::new(""), output).map_err(cmd::CmdError::from)?;
            } else {
                return Err(CliError::RequiresRuntimeHome { command: "daemon" });
            }
        }
        RootCommand::Metrics(_) => {
            return Err(CliError::RequiresMetricsQueryService);
        }
        RootCommand::Learning(command) => {
            return Err(CliError::RequiresLearningReader {
                command: command.name(),
            });
        }
        RootCommand::AuditScorecard(_) => {
            return Err(CliError::RequiresRuntimeHome { command: "audit" });
        }
        RootCommand::BarokStatus => cmd::run_barok_status(output)?,
        RootCommand::DisciplineStatus => cmd::run_discipline_status(output)?,
        #[cfg(all(unix, feature = "verification-process-canary"))]
        RootCommand::HermeticCanary => {
            cmd::run_hermetic_canary(output).map_err(cmd::CmdError::from)?;
        }
    }
    Ok(())
}

enum RootCommand {
    RootHelp,
    Run(Vec<String>),
    /// `status`, with cobra's `-h`/`--help` disposition resolved by
    /// [`split_status_command`] rather than swallowed as a positional.
    Status {
        help: bool,
    },
    Events(cmd::EventsCommand),
    Daemon(cmd::DaemonCommand),
    Metrics(cmd::MetricsCommand),
    Learning(cmd::LearningCommand),
    AuditScorecard(cmd::AuditScorecardFlags),
    BarokStatus,
    DisciplineStatus,
    #[cfg(all(unix, feature = "verification-process-canary"))]
    HermeticCanary,
}

fn split_root_command(arguments: &[String]) -> Result<RootCommand, CliError> {
    let mut usage = HELP;
    split_root_command_with_usage(arguments, &mut usage)
}

/// [`split_root_command`], reporting which usage block a refusal belongs to.
///
/// Cobra prints the *refusing command's* usage, not the root's. Every
/// command this function resolves itself refuses with the root block, so
/// `usage` starts at [`HELP`]; a sub-parser that owns its own block — today
/// only [`split_status_command`] — overwrites it before returning `Err`.
fn split_root_command_with_usage(
    arguments: &[String],
    usage: &mut &'static str,
) -> Result<RootCommand, CliError> {
    if arguments.is_empty() {
        return Ok(RootCommand::RootHelp);
    }
    let mut root_flags = Vec::new();
    let mut deferred_root_unknown = None;
    let mut force_root_help = false;
    let mut cursor = 0;
    while cursor < arguments.len() {
        let argument = &arguments[cursor];
        if force_root_help && !argument.starts_with('-') {
            cursor += 1;
            continue;
        }
        match argument.as_str() {
            "--" => {
                if let Some(name) = deferred_root_unknown {
                    return Err(CliError::RootFlag(run::RunResolveError::UnknownFlag(name)));
                }
                validate_root_flags(&root_flags)?;
                return Ok(RootCommand::RootHelp);
            }
            "--help" | "-h" => force_root_help = true,
            "help" => {
                return split_help_command(root_flags, &arguments[cursor + 1..]);
            }
            "run" => {
                root_flags.extend_from_slice(&arguments[cursor + 1..]);
                return Ok(RootCommand::Run(root_flags));
            }
            "status" => {
                *usage = STATUS_HELP;
                return split_status_command(&arguments[cursor + 1..], &root_flags);
            }
            "events" => {
                let command = cmd::parse_events(&arguments[cursor + 1..])?;
                return Ok(RootCommand::Events(command));
            }
            "daemon" => {
                let command =
                    cmd::parse_daemon(&arguments[cursor + 1..]).map_err(cmd::CmdError::from)?;
                return Ok(RootCommand::Daemon(command));
            }
            "metrics" => {
                let command = cmd::parse_metrics(&arguments[cursor + 1..])?;
                return Ok(RootCommand::Metrics(command));
            }
            // Go registers these four at the root, not under a parent
            // (`learn.go:60-62`, `archive.go:31`), so they are matched here
            // rather than behind a `learning` prefix.
            "stats" | "prune" | "archive" | "backfill-embeddings" => {
                validate_root_flags(&root_flags)?;
                let command = cmd::parse_learning(argument, &arguments[cursor + 1..])?;
                return Ok(RootCommand::Learning(command));
            }
            "audit" => {
                return split_audit_command(&arguments[cursor + 1..], &root_flags);
            }
            "barok" => {
                return split_static_status_command(
                    &arguments[cursor + 1..],
                    RootCommand::BarokStatus,
                    &root_flags,
                );
            }
            "discipline" => {
                return split_static_status_command(
                    &arguments[cursor + 1..],
                    RootCommand::DisciplineStatus,
                    &root_flags,
                );
            }
            #[cfg(all(unix, feature = "verification-process-canary"))]
            "hermetic-canary" => {
                validate_root_flags(&root_flags)?;
                return Ok(RootCommand::HermeticCanary);
            }
            "-" | "" => {}
            _ if forward_root_flag(arguments, &mut cursor, &mut root_flags)? => {}
            value if value.starts_with("--") => {
                let name = long_flag_name(value).to_owned();
                if force_root_help {
                    return Err(CliError::RootFlag(run::RunResolveError::UnknownFlag(name)));
                }
                deferred_root_unknown.get_or_insert(name);
                root_flags.push(value.to_owned());
                if !value.contains('=') {
                    if let Some(flag_value) = arguments.get(cursor + 1) {
                        root_flags.push(flag_value.clone());
                        cursor += 1;
                    }
                }
            }
            value if value.starts_with('-') => {
                let flag = value.chars().nth(1).unwrap_or('-');
                return Err(CliError::RootFlag(run::RunResolveError::UnknownShorthand {
                    flag,
                    argument: value.to_owned(),
                }));
            }
            value if is_go_command(value) => return Err(CliError::UnsupportedCommand),
            value => return Err(CliError::UnknownCommand(command_label(value))),
        }
        cursor += 1;
    }
    if let Some(name) = deferred_root_unknown {
        return Err(CliError::RootFlag(run::RunResolveError::UnknownFlag(name)));
    }
    validate_root_flags(&root_flags)?;
    Ok(RootCommand::RootHelp)
}

fn split_audit_command(
    arguments: &[String],
    root_flags: &[String],
) -> Result<RootCommand, CliError> {
    let mut arguments_with_root_flags = root_flags.to_vec();
    arguments_with_root_flags.extend_from_slice(arguments);

    let mut flags = cmd::AuditScorecardFlags::default();
    let mut persistent_flags = Vec::new();
    let mut found_scorecard = false;
    let mut cursor = 0usize;
    while cursor < arguments_with_root_flags.len() {
        let argument = &arguments_with_root_flags[cursor];
        match argument.as_str() {
            "--" if found_scorecard => break,
            "--" => return Err(CliError::UnsupportedCommand),
            "-" | "" => {}
            _ if cmd::parse_audit_local_flag(
                &arguments_with_root_flags,
                &mut cursor,
                &mut flags,
            )? => {}
            _ if forward_root_flag(
                &arguments_with_root_flags,
                &mut cursor,
                &mut persistent_flags,
            )? => {}
            value if value.starts_with("--") => {
                return Err(CliError::RootFlag(run::RunResolveError::UnknownFlag(
                    long_flag_name(value).to_owned(),
                )));
            }
            value if value.starts_with('-') => {
                let flag = value.chars().nth(1).unwrap_or('-');
                return Err(CliError::RootFlag(run::RunResolveError::UnknownShorthand {
                    flag,
                    argument: value.to_owned(),
                }));
            }
            "scorecard" if !found_scorecard => found_scorecard = true,
            _ if found_scorecard => {}
            _ => return Err(CliError::UnsupportedCommand),
        }
        cursor += 1;
    }

    if !found_scorecard {
        return Err(CliError::UnsupportedCommand);
    }
    let (persistent, _) = run::parse(&persistent_flags).map_err(CliError::RootFlag)?;
    if persistent.help {
        return Err(CliError::UnsupportedCommand);
    }
    Ok(RootCommand::AuditScorecard(flags))
}

fn bind_audit_target(home: &Path) -> Result<Option<ReadOnlyEventLogTarget>, cmd::CmdError> {
    let authority = ReadOnlyEventLogAuthority::acquire_ambient().map_err(cmd::audit_open_error)?;
    let root = match authority.bind_runtime_home(home) {
        Ok(root) => root,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(cmd::audit_open_error(source).into()),
    };
    match root.event_log(Path::new("audits.jsonl")) {
        Ok(target) => Ok(Some(target)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(cmd::audit_open_error(source).into()),
    }
}

/// Parses `orchestrator status`'s own argument list in cobra's shape.
///
/// `statusCmd` registers no local flags beyond `-h/--help` and sets no `Args`
/// validator, so cobra accepts and ignores every positional argument, accepts
/// the persistent flags, and rejects anything else that looks like a flag with
/// `unknown flag: <name>` over the command's usage block. Returning
/// `RootCommand::Status` the moment the word appeared — which is what this
/// function replaces — swallowed both `--help` and unknown flags as trailing
/// positionals, so `orchestrator status --definitely-not-a-flag` exited 0 where
/// the oracle exits 1.
fn split_status_command(
    arguments: &[String],
    root_flags: &[String],
) -> Result<RootCommand, CliError> {
    let mut persistent_flags = root_flags.to_vec();
    let mut cursor = 0usize;
    while cursor < arguments.len() {
        let argument = &arguments[cursor];
        match argument.as_str() {
            // Cobra stops parsing flags at `--`; everything after it is a
            // positional, and `statusCmd` ignores positionals.
            "--" => break,
            // `--help` is not a persistent flag `forward_root_flag` forwards,
            // so it is normalized here the way `split_help_command` does.
            "--help" => persistent_flags.push("--help=true".to_owned()),
            "-" | "" => {}
            _ if forward_root_flag(arguments, &mut cursor, &mut persistent_flags)? => {}
            value if value.starts_with("--") => {
                return Err(CliError::RootFlag(run::RunResolveError::UnknownFlag(
                    long_flag_name(value).to_owned(),
                )));
            }
            value if value.starts_with('-') => {
                let flag = value.chars().nth(1).unwrap_or('-');
                return Err(CliError::RootFlag(run::RunResolveError::UnknownShorthand {
                    flag,
                    argument: value.to_owned(),
                }));
            }
            // Accepted and ignored, exactly as cobra does with no `Args`
            // validator.
            _ => {}
        }
        cursor += 1;
    }
    let (flags, _) = run::parse(&persistent_flags).map_err(CliError::RootFlag)?;
    Ok(RootCommand::Status { help: flags.help })
}

fn split_static_status_command(
    arguments: &[String],
    status_command: RootCommand,
    root_flags: &[String],
) -> Result<RootCommand, CliError> {
    let mut persistent_flags = root_flags.to_vec();
    let mut found_status = false;
    let mut cursor = 0usize;
    while cursor < arguments.len() {
        let argument = &arguments[cursor];
        match argument.as_str() {
            "-h" | "--help" => return Err(CliError::UnsupportedCommand),
            "--" if found_status => break,
            "--" => return Err(CliError::UnsupportedCommand),
            "-" | "" => {}
            _ if forward_root_flag(arguments, &mut cursor, &mut persistent_flags)? => {}
            value if value.starts_with("--") => {
                return Err(CliError::RootFlag(run::RunResolveError::UnknownFlag(
                    long_flag_name(value).to_owned(),
                )));
            }
            value if value.starts_with('-') => {
                let flag = value.chars().nth(1).unwrap_or('-');
                return Err(CliError::RootFlag(run::RunResolveError::UnknownShorthand {
                    flag,
                    argument: value.to_owned(),
                }));
            }
            "status" if !found_status => found_status = true,
            _ if found_status => {}
            _ => return Err(CliError::UnsupportedCommand),
        }
        cursor += 1;
    }
    if !found_status {
        return Err(CliError::UnsupportedCommand);
    }
    let (flags, _) = run::parse(&persistent_flags).map_err(CliError::RootFlag)?;
    if flags.help {
        return Err(CliError::UnsupportedCommand);
    }
    Ok(status_command)
}

fn split_help_command(
    mut root_flags: Vec<String>,
    arguments: &[String],
) -> Result<RootCommand, CliError> {
    let mut cursor = 0;
    while cursor < arguments.len() {
        match arguments[cursor].as_str() {
            "run" => {
                root_flags.extend_from_slice(&arguments[cursor + 1..]);
                root_flags.push("--help=true".to_owned());
                return Ok(RootCommand::Run(root_flags));
            }
            "daemon" => {
                validate_root_flags(&root_flags)?;
                let mut daemon_arguments = arguments[cursor + 1..].to_vec();
                daemon_arguments.push("--help".to_owned());
                return Ok(RootCommand::Daemon(
                    cmd::parse_daemon(&daemon_arguments).map_err(cmd::CmdError::from)?,
                ));
            }
            _ if forward_root_flag(arguments, &mut cursor, &mut root_flags)? => {}
            value if value.starts_with("--") => {
                return Err(CliError::RootFlag(run::RunResolveError::UnknownFlag(
                    long_flag_name(value).to_owned(),
                )));
            }
            value if value.starts_with('-') => {
                let flag = value.chars().nth(1).unwrap_or('-');
                return Err(CliError::RootFlag(run::RunResolveError::UnknownShorthand {
                    flag,
                    argument: value.to_owned(),
                }));
            }
            _ => return Err(CliError::UnsupportedCommand),
        }
        cursor += 1;
    }
    validate_root_flags(&root_flags)?;
    Ok(RootCommand::RootHelp)
}

fn forward_root_flag(
    arguments: &[String],
    cursor: &mut usize,
    forwarded: &mut Vec<String>,
) -> Result<bool, CliError> {
    let argument = &arguments[*cursor];
    match argument.as_str() {
        "-v" => forwarded.push("--verbose".to_owned()),
        "--verbose" | "--dry-run" | "--sequential" => forwarded.push(argument.clone()),
        "--domain" | "--model" | "--nanika-dir" | "--personas-dir" | "--max-turns" => {
            let Some(value) = arguments.get(*cursor + 1) else {
                return Err(CliError::RootFlag(run::RunResolveError::FlagNeedsValue {
                    flag: long_flag_name(argument).to_owned(),
                }));
            };
            forwarded.push(argument.clone());
            forwarded.push(value.clone());
            *cursor += 1;
        }
        value if is_inline_root_flag(value) => forwarded.push(argument.clone()),
        value if value.starts_with("-v=") || value.starts_with("-h=") => {
            forwarded.push(argument.clone());
        }
        value if value.starts_with('-') && !value.starts_with("--") && value != "-" => {
            let shorthand = value.strip_prefix('-').unwrap_or(value);
            let cluster = shorthand
                .split_once('=')
                .map_or(shorthand, |(cluster, _)| cluster);
            for (offset, flag) in cluster.char_indices() {
                match flag {
                    'v' | 'h' => {}
                    _ => {
                        return Err(CliError::RootFlag(run::RunResolveError::UnknownShorthand {
                            flag,
                            argument: format!("-{}", &cluster[offset..]),
                        }));
                    }
                }
            }
            forwarded.push(value.to_owned());
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn validate_root_flags(arguments: &[String]) -> Result<(), CliError> {
    run::parse(arguments)
        .map(|_| ())
        .map_err(CliError::RootFlag)
}

fn is_inline_root_flag(value: &str) -> bool {
    let Some((name, _)) = value
        .strip_prefix("--")
        .and_then(|rest| rest.split_once('='))
    else {
        return false;
    };
    matches!(
        name,
        "domain"
            | "model"
            | "nanika-dir"
            | "personas-dir"
            | "max-turns"
            | "verbose"
            | "dry-run"
            | "sequential"
            | "help"
    )
}

fn long_flag_name(value: &str) -> &str {
    let stripped = value.strip_prefix("--").unwrap_or(value);
    stripped.split_once('=').map_or(stripped, |(name, _)| name)
}

fn is_go_command(value: &str) -> bool {
    matches!(
        value,
        "advisor"
            | "barok"
            | "cancel"
            | "cleanup"
            | "compare"
            | "completion"
            | "daemon"
            | "discipline"
            | "doctor"
            | "dream"
            | "evidence"
            | "hooks"
            | "ingest"
            | "memory"
            | "routing"
            | "sync"
            | "templates"
    )
}

fn command_label(value: &str) -> String {
    if value.len() <= 64
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    {
        value.to_owned()
    } else {
        "[redacted]".to_owned()
    }
}

fn write_usage(help: &str, output: &mut impl Write) -> io::Result<()> {
    const MARKER: &str = "Usage:\n";
    let usage = help.find(MARKER).map_or(help, |index| &help[index..]);
    output.write_all(usage.as_bytes())?;
    output.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::{CliError, run, run_system};

    #[test]
    fn bundled_contract_fixture_loads_for_library_conformance_tests()
    -> Result<(), Box<dyn std::error::Error>> {
        let ledger = orchestrator_app::load_bundled_conformance_ledger()?;
        assert_eq!(ledger.ledger_id, "orchestrator-go-to-rust");
        assert!(!ledger.contracts.is_empty());
        Ok(())
    }

    #[test]
    fn injected_dispatch_does_not_expose_contract_summary() {
        let mut output = Vec::new();
        assert!(matches!(
            run(["contract-summary"], &mut output),
            Err(super::CliError::UnknownCommand(command)) if command == "contract-summary"
        ));
    }

    #[test]
    fn help_does_not_resolve_or_prepare_a_runtime_home() -> Result<(), Box<dyn std::error::Error>> {
        let mut output = Vec::new();
        run(["--help"], &mut output)?;
        assert!(!output.is_empty());
        Ok(())
    }

    #[test]
    fn injected_metrics_requires_query_authority_not_runtime_home() {
        let mut output = Vec::new();
        assert!(matches!(
            run(["metrics", "trends"], &mut output),
            Err(CliError::RequiresMetricsQueryService)
        ));
        assert!(output.is_empty());
    }

    #[test]
    fn injected_audit_scorecard_requires_runtime_home_after_valid_go_flag_parsing() {
        for arguments in [
            &["audit", "scorecard"][..],
            &["--last=0x2", "audit", "scorecard"][..],
            &["audit", "--last=02", "scorecard"][..],
            &["audit", "scorecard", "--last", "+2"][..],
            &["audit", "scorecard", "--last=2_0"][..],
            &["--domain", "work", "audit", "scorecard", "--domain", "dev"][..],
            &["-", "audit", "-", "scorecard", "ignored"][..],
        ] {
            let mut output = Vec::new();
            assert!(matches!(
                run(arguments.iter().copied(), &mut output),
                Err(CliError::RequiresRuntimeHome { command: "audit" })
            ));
            assert!(output.is_empty());
        }
    }

    #[test]
    fn audit_scorecard_rejects_invalid_go_int_and_defers_help_surfaces() {
        for arguments in [
            &["audit", "scorecard", "--last=08"][..],
            &["audit", "scorecard", "--last=9223372036854775808"][..],
            &["--last", "audit", "scorecard"][..],
            &["audit", "--last", "scorecard"][..],
            &["audit", "scorecard", "--help"][..],
            &["audit"][..],
        ] {
            let mut output = Vec::new();
            assert!(run(arguments.iter().copied(), &mut output).is_err());
            assert!(output.is_empty());
        }
    }

    #[test]
    fn persistent_flags_before_run_are_dispatched_to_the_run_command()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut output = Vec::new();
        run(
            ["--domain", "work", "-v", "run", "inspect the tree"],
            &mut output,
        )?;
        let text = String::from_utf8(output)?;
        assert!(text.contains("task: inspect the tree"));
        Ok(())
    }

    #[test]
    fn run_help_is_available_with_global_flags_on_either_side()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut before = Vec::new();
        run(["--domain", "work", "run", "--help"], &mut before)?;
        let mut after = Vec::new();
        run(["run", "-v", "-h"], &mut after)?;
        assert_eq!(before, after);
        Ok(())
    }

    #[test]
    fn cobra_style_help_run_alias_matches_run_help() -> Result<(), Box<dyn std::error::Error>> {
        let mut alias = Vec::new();
        run(["help", "run"], &mut alias)?;
        let mut direct = Vec::new();
        run(["run", "--help"], &mut direct)?;
        assert_eq!(alias, direct);
        Ok(())
    }

    #[test]
    fn persistent_boolean_equals_forms_work_before_run() -> Result<(), Box<dyn std::error::Error>> {
        let mut output = Vec::new();
        run(
            ["--verbose=false", "--dry-run=true", "run", "inspect"],
            &mut output,
        )?;
        assert!(String::from_utf8(output)?.contains("task: inspect"));
        Ok(())
    }

    #[test]
    fn static_status_commands_dispatch_without_runtime_home()
    -> Result<(), Box<dyn std::error::Error>> {
        for (arguments, heading) in [
            (
                &["barok", "status", "ignored-positional"][..],
                "barok output-compression status\n",
            ),
            (
                &["discipline", "status", "ignored-positional"][..],
                "reasoning-discipline status\n",
            ),
            (
                &["barok", "status", "-"][..],
                "barok output-compression status\n",
            ),
            (
                &["discipline", "status", "-"][..],
                "reasoning-discipline status\n",
            ),
            (
                &["barok", "-", "status"][..],
                "barok output-compression status\n",
            ),
            (
                &["discipline", "-", "status"][..],
                "reasoning-discipline status\n",
            ),
            (
                &["barok", "", "status"][..],
                "barok output-compression status\n",
            ),
            (
                &["discipline", "", "status"][..],
                "reasoning-discipline status\n",
            ),
        ] {
            let mut output = Vec::new();
            run(arguments.iter().copied(), &mut output)?;
            assert!(String::from_utf8(output)?.starts_with(heading));
        }
        Ok(())
    }

    #[test]
    fn system_and_injected_static_status_dispatch_match() -> Result<(), Box<dyn std::error::Error>>
    {
        for arguments in [&["barok", "status"][..], &["discipline", "status"][..]] {
            let mut injected = Vec::new();
            run(arguments.iter().copied(), &mut injected)?;
            let mut system = Vec::new();
            let mut system_error = Vec::new();
            run_system(arguments.iter().copied(), &mut system, &mut system_error)?;
            assert_eq!(system, injected);
            assert!(system_error.is_empty());
        }
        Ok(())
    }

    #[test]
    fn static_status_accepts_inherited_flags_before_between_and_after_commands()
    -> Result<(), Box<dyn std::error::Error>> {
        for (arguments, heading) in [
            (
                &["--domain", "work", "barok", "status"][..],
                "barok output-compression status\n",
            ),
            (
                &["barok", "--domain", "work", "status"][..],
                "barok output-compression status\n",
            ),
            (
                &["barok", "status", "--domain", "work"][..],
                "barok output-compression status\n",
            ),
            (
                &["-v", "discipline", "status"][..],
                "reasoning-discipline status\n",
            ),
            (
                &["discipline", "-v", "status"][..],
                "reasoning-discipline status\n",
            ),
            (
                &["discipline", "status", "-v"][..],
                "reasoning-discipline status\n",
            ),
        ] {
            let mut output = Vec::new();
            run(arguments.iter().copied(), &mut output)?;
            assert!(String::from_utf8(output)?.starts_with(heading));
        }
        Ok(())
    }

    #[test]
    fn static_status_help_is_explicitly_deferred_without_executing_status() {
        for arguments in [
            &["barok", "status", "--help"][..],
            &["discipline", "status", "-h"][..],
        ] {
            let mut output = Vec::new();
            assert!(matches!(
                run(arguments.iter().copied(), &mut output),
                Err(CliError::UnsupportedCommand)
            ));
            assert!(output.is_empty());
        }
    }

    #[test]
    fn status_rejects_unknown_flags_and_serves_its_own_help_without_a_runtime_home() {
        for arguments in [
            &["status", "--definitely-not-a-flag"][..],
            &["status", "-Z"][..],
        ] {
            let mut output = Vec::new();
            assert!(matches!(
                run(arguments.iter().copied(), &mut output),
                Err(CliError::RootFlag(_))
            ));
            assert!(output.is_empty());
        }

        for arguments in [&["status", "--help"][..], &["status", "-h"][..]] {
            let mut output = Vec::new();
            assert!(run(arguments.iter().copied(), &mut output).is_ok());
            assert_eq!(String::from_utf8_lossy(&output), super::STATUS_HELP);
        }

        // No `Args` validator in Go, so positionals are accepted and ignored —
        // the command still needs a runtime home to answer.
        let mut output = Vec::new();
        assert!(matches!(
            run(["status", "left", "over"], &mut output),
            Err(CliError::RequiresRuntimeHome { command: "status" })
        ));
        assert!(output.is_empty());
    }

    #[test]
    fn static_status_bare_groups_and_unknown_flags_are_not_silently_accepted() {
        for arguments in [
            &["barok"][..],
            &["discipline"][..],
            &["barok", "status", "--unknown"][..],
            &["discipline", "status", "-x"][..],
        ] {
            let mut output = Vec::new();
            assert!(run(arguments.iter().copied(), &mut output).is_err());
            assert!(output.is_empty());
        }
    }
}
