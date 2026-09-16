//! Resolution front-end for `orchestrator run`.
//!
//! This is the dependency-ordered first slice of live `run`: it parses the run
//! command's arguments and flags (ORC-CLI-ROOT-001 persistent flags and
//! ORC-CLI-RUN-001 run flags), validates their combinations, loads the task or
//! mission file, and plans any predecomposed `PHASE:` records (ORC-DECOMPOSE-001).
//!
//! It performs no provider execution, workspace admission, git isolation, or
//! live-engine work; those are later slices. It is pure except for reading a
//! mission file named on the command line.

use orchestrator_app::{GitEffectError, GitEffectService, GitIntent, GitReceipt, TrashRoot};
use orchestrator_core::{
    AuthoredParseContext, AuthoredPhase, ConfigError, ExecutionMode, MissionParseError,
    ModelResolutionInput, ModelTier, PersonaCatalog, PhaseId, ProposalError, RoutingMap,
    RuntimePolicyFixture, RuntimeResolutionInput, RuntimeSource, StallResolutionInput, StallSource,
    StallTimeoutResolution, StallTimeoutValue, TargetContextFixture, compile_proposal,
    keyword_fallback_proposal, parse_authored_phases, resolve_effort_for_runtime, resolve_model,
    resolve_model_for_runtime, resolve_runtime, resolve_stall_timeout, select_runtime,
};
use orchestrator_exec::{
    AttemptOutcome, ContractError, DispatchError, Effort, ExecutionContext, ExecutionRequest,
    ExecutionRequestDraft, ExecutorRegistry, RuntimeFamily, RuntimeRegistryError, SessionHandle,
};
use orchestrator_git::PushAck;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

const DEFAULT_DOMAIN: &str = "dev";
const DEFAULT_GATE_MODE: &str = "block";

/// Persistent root flags accepted before or after every command (ORC-CLI-ROOT-001).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistentFlags {
    pub verbose: bool,
    pub dry_run: bool,
    pub domain: String,
    pub model: String,
    pub sequential: bool,
    pub personas_dir: Option<PathBuf>,
    pub nanika_dir: Option<PathBuf>,
    pub max_turns: Option<i64>,
}

impl Default for PersistentFlags {
    fn default() -> Self {
        Self {
            verbose: false,
            dry_run: false,
            domain: DEFAULT_DOMAIN.to_owned(),
            model: String::new(),
            sequential: false,
            personas_dir: None,
            nanika_dir: None,
            max_turns: None,
        }
    }
}

/// Flags for `orchestrator run` (ORC-CLI-RUN-001), with documented defaults.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunFlags {
    pub persistent: PersistentFlags,
    pub help: bool,
    pub resume: Option<PathBuf>,
    pub no_learnings: bool,
    pub no_review: bool,
    pub review_runtime: Option<String>,
    pub runtime: Option<String>,
    pub gate_mode: String,
    pub template: Option<String>,
    pub save_template: Option<String>,
    pub git_isolate: bool,
    pub no_git: bool,
    pub pr: bool,
    pub no_draft: bool,
    pub codex_review: bool,
    pub no_comment: bool,
    pub stall_timeout: Option<String>,
    pub no_persistent_worker: bool,
    /// Compile and dispatch one released phase against the local executor
    /// protocol (no live provider). Wires the keyword-fallback decomposer and
    /// the `run_one_phase` dispatch seam; multi-phase and non-offline runs stay
    /// enrollment-gated.
    pub offline: bool,
}

impl Default for RunFlags {
    fn default() -> Self {
        Self {
            persistent: PersistentFlags::default(),
            help: false,
            resume: None,
            no_learnings: false,
            no_review: false,
            review_runtime: None,
            runtime: None,
            gate_mode: DEFAULT_GATE_MODE.to_owned(),
            template: None,
            save_template: None,
            git_isolate: true,
            no_git: false,
            pr: false,
            no_draft: false,
            codex_review: false,
            no_comment: false,
            stall_timeout: None,
            no_persistent_worker: false,
            offline: false,
        }
    }
}

/// Environment-derived inputs that feed runtime routing. Passed in so resolution
/// stays deterministic and testable; the CLI populates it from the real environment.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EnvInputs {
    /// `NANIKA_DEFAULT_RUNTIME`.
    pub default_runtime: Option<String>,
    /// `NANIKA_CODEX_AUTO` (empty when unset).
    pub codex_auto: String,
    /// `ORCHESTRATOR_STALL_TIMEOUT`, retained even when empty so invalid
    /// explicit configuration cannot silently become the worker default.
    pub stall_timeout: Option<String>,
    /// `ORCHESTRATOR_NANIKA_DIR`.
    pub nanika_dir: Option<PathBuf>,
    /// Legacy `ORCHESTRATOR_VIA_DIR`.
    pub legacy_via_dir: Option<PathBuf>,
}

impl EnvInputs {
    /// Reads the routing and Nanika-directory environment variables the Go
    /// entrypoint consults. Path values remain `OsString`-backed.
    pub fn from_environment() -> Self {
        Self {
            default_runtime: std::env::var("NANIKA_DEFAULT_RUNTIME")
                .ok()
                .filter(|value| !value.is_empty()),
            codex_auto: std::env::var("NANIKA_CODEX_AUTO").unwrap_or_default(),
            stall_timeout: std::env::var("ORCHESTRATOR_STALL_TIMEOUT").ok(),
            nanika_dir: optional_environment_path("ORCHESTRATOR_NANIKA_DIR"),
            legacy_via_dir: optional_environment_path("ORCHESTRATOR_VIA_DIR"),
        }
    }
}

fn optional_environment_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// One phase with its resolved execution parameters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPhase {
    pub id: PhaseId,
    pub name: String,
    pub objective: String,
    pub persona: String,
    pub persona_selection_method: String,
    pub role: String,
    pub skills: Vec<String>,
    pub dependencies: Vec<PhaseId>,
    pub expected: String,
    pub workdir: String,
    pub authored_runtime: String,
    pub runtime_policy_applied: bool,
    pub authored_model_tier: String,
    pub tier: ModelTier,
    pub effective_runtime: String,
    pub runtime_source: RuntimeSource,
    pub model: String,
    pub effort: String,
    pub authored_stall_timeout: Option<Duration>,
    pub stall_timeout: Option<Duration>,
    pub stall_timeout_source: StallSource,
    pub priority: String,
    pub status: String,
}

/// Deterministic inputs supplied by the application composition root.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RunResolutionContext {
    pub environment: EnvInputs,
    pub user_home: Option<PathBuf>,
    pub personas: PersonaCatalog,
    pub routing: RoutingMap,
    pub policy: RuntimePolicyFixture,
    pub target: Option<TargetContextFixture>,
}

/// The resolved `run` input after parsing, validation, and per-phase routing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedRun {
    pub flags: RunFlags,
    pub task: String,
    pub mission_path: Option<PathBuf>,
    /// Empty for a single-worker task; populated for predecomposed missions.
    pub phases: Vec<ResolvedPhase>,
    pub execution_mode: ExecutionMode,
    pub default_stall_timeout: StallTimeoutResolution,
    /// Effective Nanika root for worker state and skill/plugin discovery.
    pub nanika_dir: PathBuf,
}

/// Failures produced while resolving a `run` command.
#[derive(Debug, Error)]
pub enum RunResolveError {
    #[error("provide a task or use --template <name>")]
    MissingTask,
    #[error("--pr requires git isolation: cannot combine with --no-git or --git-isolate=false")]
    PrRequiresGitIsolation,
    #[error("--codex-review requires --pr")]
    CodexReviewRequiresPr,
    #[error("unknown flag: --{0}")]
    UnknownFlag(String),
    #[error("unknown shorthand flag: {flag:?} in {argument}")]
    UnknownShorthand { flag: char, argument: String },
    #[error("flag needs an argument: --{flag}")]
    FlagNeedsValue { flag: String },
    #[error(
        "invalid argument {value:?} for \"--{flag}\" flag: strconv.ParseBool: parsing {value:?}: invalid syntax"
    )]
    InvalidBool { flag: String, value: String },
    #[error(
        "invalid argument {value:?} for \"-{shorthand}, --{flag}\" flag: strconv.ParseBool: parsing {value:?}: invalid syntax"
    )]
    InvalidShorthandBool {
        flag: String,
        shorthand: char,
        value: String,
    },
    #[error(
        "invalid argument {value:?} for \"--{flag}\" flag: strconv.ParseInt: parsing {value:?}: invalid syntax"
    )]
    InvalidCount { flag: String, value: String },
    #[error("--gate-mode must be \"warn\" or \"block\", got {0:?}")]
    InvalidGateMode(String),
    #[error(transparent)]
    Configuration(#[from] ConfigError),
    #[error("read mission file: {0}")]
    ReadMissionFile(String),
    #[error("invalid authored mission: {0}")]
    AuthoredMission(#[from] MissionParseError),
    /// A recognized flag path whose backing implementation is a later slice.
    #[error("{0} is not implemented in this slice of the Rust orchestrator")]
    NotYetImplemented(&'static str),
    /// The offline keyword-fallback proposal failed to compile into a plan.
    #[error("cannot compile the offline plan: {0}")]
    Proposal(#[from] ProposalError),
}

/// Parses `orchestrator run` arguments (the slice after the `run` subcommand)
/// using the live environment for routing variables. Prefer [`resolve_with`] in
/// tests so routing is deterministic.
pub fn resolve(arguments: &[String]) -> Result<ResolvedRun, RunResolveError> {
    resolve_with_context(
        arguments,
        &RunResolutionContext {
            environment: EnvInputs::from_environment(),
            user_home: optional_environment_path("HOME"),
            ..RunResolutionContext::default()
        },
    )
}

/// Deterministic variant of [`resolve`] that takes the routing environment as
/// an explicit input.
#[cfg(test)]
pub fn resolve_with(arguments: &[String], env: &EnvInputs) -> Result<ResolvedRun, RunResolveError> {
    resolve_with_context(
        arguments,
        &RunResolutionContext {
            environment: env.clone(),
            ..RunResolutionContext::default()
        },
    )
}

/// Resolves a run from the explicit config, persona, routing, and environment
/// inputs admitted by the application layer.
pub fn resolve_with_context(
    arguments: &[String],
    context: &RunResolutionContext,
) -> Result<ResolvedRun, RunResolveError> {
    let (flags, positional) = parse(arguments)?;
    validate_invocation(&flags, &positional)?;
    let default_stall_timeout = resolve_stall_timeout(StallResolutionInput {
        phase_timeout: None,
        flag_value: flags.stall_timeout.clone(),
        environment_value: context.environment.stall_timeout.clone(),
    })?;
    let nanika_dir = resolve_nanika_dir(
        &flags.persistent,
        &context.environment,
        context.user_home.as_deref(),
    );

    let joined = positional.join(" ");
    let (task, mission_path) = load_task(&joined)?;

    let parse_context = AuthoredParseContext {
        user_home: context.user_home.clone(),
        persona_catalog: context.personas.clone(),
        target_context: context.target.clone(),
        policy: context.policy.clone(),
    };
    let (phases, execution_mode) = match parse_authored_phases(&task, &parse_context) {
        Ok(projection) if !projection.phases.is_empty() => (
            resolve_phases(
                &projection.phases,
                &flags,
                &context.environment,
                &context.routing,
            )?,
            projection.execution_mode,
        ),
        Err(error) if contains_authored_phase_record(&task) => return Err(error.into()),
        _ if flags.offline => {
            // No authored PHASE records: engage the deterministic keyword
            // fallback so an offline run always yields a compilable plan without
            // a provider. There is exactly one compile path (`compile_proposal`);
            // provenance (authored vs keyword-fallback) is metadata, not a second
            // code path.
            let proposal = keyword_fallback_proposal(&task);
            let compiled = compile_proposal(&proposal, &parse_context)?;
            let resolved_phases = resolve_phases(
                &compiled.phases,
                &flags,
                &context.environment,
                &context.routing,
            )?;
            (resolved_phases, compiled.execution_mode)
        }
        _ => (Vec::new(), ExecutionMode::Sequential),
    };

    Ok(ResolvedRun {
        flags,
        task,
        mission_path,
        phases,
        execution_mode,
        default_stall_timeout,
        nanika_dir,
    })
}

fn resolve_nanika_dir(
    flags: &PersistentFlags,
    environment: &EnvInputs,
    user_home: Option<&Path>,
) -> PathBuf {
    flags
        .nanika_dir
        .as_ref()
        .filter(|path| !path.as_os_str().is_empty())
        .cloned()
        .or_else(|| {
            environment
                .nanika_dir
                .as_ref()
                .filter(|path| !path.as_os_str().is_empty())
                .cloned()
        })
        .or_else(|| {
            environment
                .legacy_via_dir
                .as_ref()
                .filter(|path| !path.as_os_str().is_empty())
                .cloned()
        })
        .or_else(|| user_home.map(|home| home.join("nanika")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Resolves each authored phase's effective runtime, model, tier, and effort
/// via the ported router and the core precedence resolvers. Routing-map
/// (`config.yaml`) loading is a later slice; an empty map is used here.
fn resolve_phases(
    phases: &[AuthoredPhase],
    flags: &RunFlags,
    env: &EnvInputs,
    routing_map: &RoutingMap,
) -> Result<Vec<ResolvedPhase>, RunResolveError> {
    let forced_model = if flags.persistent.model.is_empty() {
        None
    } else {
        Some(flags.persistent.model.clone())
    };
    phases
        .iter()
        .map(|authored| {
            let tier = ModelTier::parse(&authored.model_tier).unwrap_or_default();
            let policy_runtime = select_runtime(
                &authored.role,
                &authored.persona,
                &authored.objective,
                &env.codex_auto,
            );
            let configured_tier_runtime = routing_map
                .model_tiers
                .get(tier.as_str())
                .map(|entry| entry.runtime.clone());
            let resolution = resolve_runtime(RuntimeResolutionInput {
                authored_runtime: Some(authored.runtime.clone()),
                runtime_policy_applied: authored.runtime_policy_applied,
                forced_runtime: flags.runtime.clone(),
                environment_runtime: env.default_runtime.clone(),
                configured_tier_runtime,
                policy_runtime: Some(policy_runtime.to_owned()),
            });
            let built_in = resolve_model_for_runtime(tier, &resolution.runtime);
            let model = resolve_model(ModelResolutionInput {
                forced_model: forced_model.clone(),
                tier: tier.to_string(),
                effective_runtime: resolution.runtime.clone(),
                routing_map: routing_map.clone(),
                built_in_model: built_in.to_owned(),
            });
            let effort =
                resolve_effort_for_runtime(tier, &authored.persona, &resolution.runtime).to_owned();
            let stall_timeout = resolve_stall_timeout(StallResolutionInput {
                phase_timeout: authored.stall_timeout,
                flag_value: flags.stall_timeout.clone(),
                environment_value: env.stall_timeout.clone(),
            })?;
            let effective_stall_timeout = match stall_timeout.value {
                StallTimeoutValue::Duration(duration) => Some(duration),
                StallTimeoutValue::WorkerDefault => None,
            };
            Ok(ResolvedPhase {
                id: authored.id.clone(),
                name: authored.name.clone(),
                objective: authored.objective.clone(),
                persona: authored.persona.clone(),
                persona_selection_method: authored.persona_selection_method.clone(),
                role: authored.role.clone(),
                skills: authored.skills.clone(),
                dependencies: authored.dependencies.clone(),
                expected: authored.expected.clone(),
                workdir: authored.workdir.clone(),
                authored_runtime: authored.runtime.clone(),
                runtime_policy_applied: authored.runtime_policy_applied,
                authored_model_tier: authored.model_tier.clone(),
                tier,
                effective_runtime: resolution.runtime,
                runtime_source: resolution.source,
                model,
                effort,
                authored_stall_timeout: authored.stall_timeout,
                stall_timeout: effective_stall_timeout,
                stall_timeout_source: stall_timeout.source,
                priority: authored.priority.clone(),
                status: authored.status.clone(),
            })
        })
        .collect()
}

fn contains_authored_phase_record(source: &str) -> bool {
    source
        .lines()
        .map(str::trim)
        .map(|line| line.trim_matches('`'))
        .any(|line| line.starts_with("PHASE:"))
}

fn validate(flags: &RunFlags) -> Result<(), RunResolveError> {
    if flags.pr && (flags.no_git || !flags.git_isolate) {
        return Err(RunResolveError::PrRequiresGitIsolation);
    }
    if flags.codex_review && !flags.pr {
        return Err(RunResolveError::CodexReviewRequiresPr);
    }
    if !matches!(flags.gate_mode.as_str(), "warn" | "block") {
        return Err(RunResolveError::InvalidGateMode(flags.gate_mode.clone()));
    }
    Ok(())
}

pub(super) fn validate_invocation(
    flags: &RunFlags,
    positional: &[String],
) -> Result<(), RunResolveError> {
    if flags.resume.is_none() && flags.template.is_none() && positional.is_empty() {
        return Err(RunResolveError::MissingTask);
    }
    validate(flags)?;
    if flags.resume.is_some() {
        return Err(RunResolveError::NotYetImplemented("resume"));
    }
    if flags.template.is_some() {
        return Err(RunResolveError::NotYetImplemented("template"));
    }
    Ok(())
}

/// Joins positional task words and, when the result names a `.md` file, reads it
/// verbatim as the mission source (matching the Go `run` entrypoint).
fn load_task(joined: &str) -> Result<(String, Option<PathBuf>), RunResolveError> {
    if !joined.ends_with(".md") {
        return Ok((joined.to_owned(), None));
    }
    let path = Path::new(joined);
    let bytes = std::fs::read(path).map_err(|e| RunResolveError::ReadMissionFile(e.to_string()))?;
    let task =
        String::from_utf8(bytes).map_err(|e| RunResolveError::ReadMissionFile(e.to_string()))?;
    let mission_path = path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    });
    Ok((task, Some(mission_path)))
}

/// Splits `run` arguments into parsed flags and leftover positional task words.
pub(crate) fn parse(arguments: &[String]) -> Result<(RunFlags, Vec<String>), RunResolveError> {
    let mut flags = RunFlags::default();
    let mut positional: Vec<String> = Vec::new();
    let mut no_more_flags = false;

    let mut iter = arguments.iter();
    while let Some(raw) = iter.next() {
        if no_more_flags {
            positional.push(raw.clone());
            continue;
        }
        if raw == "--" {
            no_more_flags = true;
            continue;
        }
        if raw == "-v" {
            flags.persistent.verbose = true;
            continue;
        }
        if raw == "-h" {
            flags.help = true;
            continue;
        }
        if let Some(value) = raw.strip_prefix("-v=") {
            flags.persistent.verbose = parse_shorthand_bool("verbose", 'v', Some(value))?;
            continue;
        }
        if let Some(value) = raw.strip_prefix("-h=") {
            flags.help = parse_shorthand_bool("help", 'h', Some(value))?;
            continue;
        }
        if raw.starts_with('-') && !raw.starts_with("--") && raw != "-" {
            parse_shorthand_cluster(raw, &mut flags)?;
            continue;
        }
        let Some(rest) = raw.strip_prefix("--") else {
            positional.push(raw.clone());
            continue;
        };
        let (name, inline_value) = rest
            .split_once('=')
            .map_or((rest, None), |(n, v)| (n, Some(v)));

        match name {
            "help" => {
                flags.help = parse_bool(name, inline_value)?;
            }
            // Persistent bool flags.
            "verbose" | "dry-run" | "sequential" => {
                set_persistent_bool(&mut flags, name, inline_value)?;
            }
            // Run bool flags.
            "no-learnings"
            | "no-review"
            | "git-isolate"
            | "no-git"
            | "pr"
            | "no-draft"
            | "codex-review"
            | "no-comment"
            | "no-persistent-worker"
            | "offline" => {
                set_run_bool(&mut flags, name, inline_value)?;
            }
            // Persistent value flags.
            "domain" | "model" => {
                let value = take_value(name, inline_value, &mut iter)?;
                match name {
                    "domain" => flags.persistent.domain = value,
                    "model" => flags.persistent.model = value,
                    _ => unreachable!(),
                }
            }
            "personas-dir" => {
                flags.persistent.personas_dir =
                    Some(PathBuf::from(take_value(name, inline_value, &mut iter)?));
            }
            "nanika-dir" => {
                flags.persistent.nanika_dir =
                    Some(PathBuf::from(take_value(name, inline_value, &mut iter)?));
            }
            "max-turns" => {
                let value = take_value(name, inline_value, &mut iter)?;
                let parsed = value
                    .parse::<i64>()
                    .map_err(|_| RunResolveError::InvalidCount {
                        flag: name.to_owned(),
                        value,
                    })?;
                flags.persistent.max_turns = Some(parsed);
            }
            // Run value flags.
            "resume" => {
                flags.resume = Some(PathBuf::from(take_value(name, inline_value, &mut iter)?));
            }
            "review-runtime" => {
                flags.review_runtime = Some(take_value(name, inline_value, &mut iter)?);
            }
            "runtime" => {
                flags.runtime = Some(take_value(name, inline_value, &mut iter)?);
            }
            "gate-mode" => {
                let value = take_value(name, inline_value, &mut iter)?;
                flags.gate_mode = if value.is_empty() {
                    DEFAULT_GATE_MODE.to_owned()
                } else {
                    value
                };
            }
            "template" => {
                flags.template = Some(take_value(name, inline_value, &mut iter)?);
            }
            "save-template" => {
                flags.save_template = Some(take_value(name, inline_value, &mut iter)?);
            }
            "stall-timeout" => {
                flags.stall_timeout = Some(take_value(name, inline_value, &mut iter)?);
            }
            other => return Err(RunResolveError::UnknownFlag(other.to_owned())),
        }
    }

    Ok((flags, positional))
}

fn parse_shorthand_cluster(raw: &str, flags: &mut RunFlags) -> Result<(), RunResolveError> {
    let shorthand = raw.strip_prefix('-').unwrap_or(raw);
    let (cluster, inline_value) = shorthand
        .split_once('=')
        .map_or((shorthand, None), |(cluster, value)| (cluster, Some(value)));
    let mut characters = cluster.char_indices().peekable();
    while let Some((offset, flag)) = characters.next() {
        let value = if characters.peek().is_none() {
            inline_value
        } else {
            None
        };
        match flag {
            'v' => flags.persistent.verbose = parse_shorthand_bool("verbose", 'v', value)?,
            'h' => flags.help = parse_shorthand_bool("help", 'h', value)?,
            _ => {
                return Err(RunResolveError::UnknownShorthand {
                    flag,
                    argument: format!("-{}", &cluster[offset..]),
                });
            }
        }
    }
    Ok(())
}

fn take_value<'a, I>(
    name: &str,
    inline: Option<&'a str>,
    iter: &mut I,
) -> Result<String, RunResolveError>
where
    I: Iterator<Item = &'a String>,
{
    if let Some(value) = inline {
        return Ok(value.to_owned());
    }
    iter.next()
        .cloned()
        .ok_or_else(|| RunResolveError::FlagNeedsValue {
            flag: name.to_owned(),
        })
}

fn parse_bool(name: &str, inline: Option<&str>) -> Result<bool, RunResolveError> {
    parse_bool_value(inline).map_err(|value| RunResolveError::InvalidBool {
        flag: name.to_owned(),
        value: value.to_owned(),
    })
}

fn parse_shorthand_bool(
    name: &str,
    shorthand: char,
    inline: Option<&str>,
) -> Result<bool, RunResolveError> {
    parse_bool_value(inline).map_err(|value| RunResolveError::InvalidShorthandBool {
        flag: name.to_owned(),
        shorthand,
        value: value.to_owned(),
    })
}

fn parse_bool_value(inline: Option<&str>) -> Result<bool, &str> {
    match inline {
        None => Ok(true),
        Some("1" | "t" | "T" | "TRUE" | "true" | "True") => Ok(true),
        Some("0" | "f" | "F" | "FALSE" | "false" | "False") => Ok(false),
        Some(other) => Err(other),
    }
}

fn set_persistent_bool(
    flags: &mut RunFlags,
    name: &str,
    inline: Option<&str>,
) -> Result<(), RunResolveError> {
    let value = parse_bool(name, inline)?;
    match name {
        "verbose" => flags.persistent.verbose = value,
        "dry-run" => flags.persistent.dry_run = value,
        "sequential" => flags.persistent.sequential = value,
        _ => unreachable!(),
    }
    Ok(())
}

fn set_run_bool(
    flags: &mut RunFlags,
    name: &str,
    inline: Option<&str>,
) -> Result<(), RunResolveError> {
    let value = parse_bool(name, inline)?;
    match name {
        "no-learnings" => flags.no_learnings = value,
        "no-review" => flags.no_review = value,
        "git-isolate" => flags.git_isolate = value,
        "no-git" => flags.no_git = value,
        "pr" => flags.pr = value,
        "no-draft" => flags.no_draft = value,
        "codex-review" => flags.codex_review = value,
        "no-comment" => flags.no_comment = value,
        "no-persistent-worker" => flags.no_persistent_worker = value,
        "offline" => flags.offline = value,
        _ => unreachable!(),
    }
    Ok(())
}

/// Renders a resolved run as stable human-readable text (slice 2: planning only).
/// Renders the resolved plan.
///
/// `executed` is what the sealed root actually did, not what the flags asked
/// for. Before B5 the ordinary path could never run, so "planning-only" was a
/// statement about the binary; now it is a statement about this invocation,
/// and an enrolled run that dispatched its phases must not claim otherwise.
pub fn render(
    resolved: &ResolvedRun,
    executed: bool,
    output: &mut impl std::io::Write,
) -> std::io::Result<()> {
    match &resolved.mission_path {
        Some(path) => writeln!(output, "mission: {}", path.display())?,
        None => writeln!(output, "task: {}", first_line(&resolved.task))?,
    }
    if resolved.phases.is_empty() {
        writeln!(
            output,
            "phases: single-worker (no predecomposed PHASE records; decomposition is a later slice)"
        )?;
    } else {
        let mode = match resolved.execution_mode {
            ExecutionMode::Sequential => "sequential",
            ExecutionMode::Parallel => "parallel",
        };
        writeln!(output, "phases: {} ({mode})", resolved.phases.len())?;
        for phase in &resolved.phases {
            writeln!(
                output,
                "  - {} {} [persona={} role={} tier={} runtime={} model={} effort={}]",
                phase.id,
                phase.name,
                phase.persona,
                phase.role,
                phase.tier,
                phase.effective_runtime,
                phase.model,
                phase.effort,
            )?;
        }
    }
    let execution = if resolved.flags.persistent.dry_run {
        "dry-run"
    } else if executed {
        "enrolled"
    } else {
        "planning-only API (system execution requires enrollment)"
    };
    writeln!(output, "execution: {execution}")?;
    Ok(())
}

pub(crate) fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// Failures while dispatching one released phase through the enforced executor
/// registry seam.
#[derive(Debug, Error)]
pub enum RunExecutionError {
    /// The phase's runtime could not be resolved to a registered executor.
    #[error("runtime resolution failed: {0}")]
    Registry(#[from] RuntimeRegistryError),
    /// The execution request was invalid for the resolved runtime.
    #[error("execution request was rejected: {0}")]
    Request(#[from] ContractError),
    /// Dispatch could not be bound before the executor ran.
    #[error("dispatch binding failed: {0}")]
    Dispatch(#[from] DispatchError),
}

/// Dispatches exactly one resolved phase through the enforced executor registry
/// and returns its terminal attempt outcome.
///
/// This is the one-phase execution seam that replaced the historical
/// `ExecutionNotEnrolled` dead-end. B5 enrolled it: `composition::seal`
/// registers exactly one executor and `SealedRun::execute` drives every
/// released phase through here, so the ordinary run path and the `--offline`
/// path now reach the same seam. The registry and the execution context are
/// supplied by the composition root (constructor injection): the registry is the
/// sole producer of the underlying dispatch request, and
/// `ResolvedExecutor::execute` owns the whole event lifecycle
/// (`worker.spawned` / `worker.output` / one terminal event) inside a panic
/// boundary. A raw dispatch request is never constructed here.
///
/// The request's runtime is taken from the registry's *effective* runtime so it
/// always matches what the registry enforces, even when an unknown runtime falls
/// back to the default executor.
///
/// `resume_from` threads a continuation decision's same-provider handle (see
/// `orchestrator_app::ContinuationDecision::ResumeSameProvider`) into the
/// request. `ExecutionRequest::new` rejects a handle whose family does not
/// match `runtime` (`ContractError::SessionRuntimeMismatch`), and
/// `ResolvedExecutor::execute` re-checks the same invariant plus the runtime's
/// `SessionResume` capability before dispatch — a cross-provider resume can
/// never reach the executor.
///
/// # Errors
///
/// Returns [`RunExecutionError`] if the runtime resolves to no executor, the
/// request is invalid, or dispatch fails to bind before the executor runs.
pub fn run_one_phase(
    mission_id: &str,
    domain: &str,
    phase: &ResolvedPhase,
    registry: &ExecutorRegistry,
    context: &mut ExecutionContext<'_>,
    resume_from: Option<SessionHandle>,
) -> Result<AttemptOutcome, RunExecutionError> {
    let resolved = registry.resolve(&phase.effective_runtime)?;
    let runtime = resolved.effective_runtime().clone();
    let request = build_execution_request(mission_id, domain, phase, runtime, resume_from)?;
    Ok(resolved.execute(&request, context)?)
}

// ---------------------------------------------------------------------------
// B4-DESIGN §1.5 / §6.2 — the ordinary run seam, with governed Git effects
// ---------------------------------------------------------------------------

/// Everything the governed-Git run seam needs that a resolved phase does not
/// already carry.
///
/// The base branch is a **configured input**, never "whatever is checked out".
/// That is the direct fix for TRK-1116: the Go oracle read the base from
/// `git.CurrentBranch(repoRoot)`, so a mission launched from a feature checkout
/// branched off that feature rather than off the intended base.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitRunPlan {
    pub remote: String,
    pub base_branch: String,
    pub task: String,
    pub commit_message: String,
    pub pr_title: String,
    pub pr_body: String,
    pub pr_draft: bool,
    /// A single path component beneath the capability root that trashed
    /// worktrees are renamed into.
    pub trash_component: String,
}

/// Every receipt one governed run produced, in the order it produced them.
///
/// `commit`, `push`, and `pull_request` are `None` when the phase did not
/// complete: `VerifiedWork::from_outcome` returns `None` for an incomplete
/// attempt, and `GitIntent::Commit` cannot be constructed without one, so no
/// `git-commit` row is ever journaled for unverified work.
#[derive(Debug)]
pub struct GitRunReceipts {
    pub base: GitReceipt,
    pub isolation: GitReceipt,
    pub outcome: AttemptOutcome,
    pub commit: Option<GitReceipt>,
    pub push: Option<GitReceipt>,
    pub pull_request: Option<GitReceipt>,
    pub cleanup: Option<GitReceipt>,
}

/// The governed-Git half of one run: the service that owns every effect and
/// the plan that parameterizes them.
///
/// Bundled rather than passed as two more arguments so the seam's signature
/// stays the dispatch inputs plus one Git binding.
pub struct GovernedGit<'binding, 'enrolment> {
    pub service: &'binding mut GitEffectService<'enrolment>,
    pub plan: &'binding GitRunPlan,
}

/// Failures from the governed-Git run seam.
#[derive(Debug, Error)]
pub enum GitRunError {
    #[error(transparent)]
    Execution(#[from] RunExecutionError),
    #[error(transparent)]
    GitEffect(#[from] GitEffectError),
    /// A completed `RefreshBase` did not yield the base it resolved to, so no
    /// isolation may be cut.
    #[error("the governed refresh produced no base to branch from")]
    NoRefreshedBase,
    /// The caller's mission id is not a well-formed [`MissionId`]. This is an
    /// input fault, not a statement about the refreshed base, and reporting it
    /// as [`Self::NoRefreshedBase`] sent an operator to look at the remote for
    /// a defect that is in the argv.
    #[error("the mission id is not well formed")]
    InvalidMissionId,
}

/// Drives one released phase with governed Git effects on either side of it.
///
/// `RefreshBase` and `CreateIsolation` run **before** dispatch;
/// `Commit`, `Push`, `OpenPr`, and `Cleanup` run after it. The service is held
/// here and never handed to the worker — the worker's `ExecutionContext`
/// carries `DeniedGitMutationService`, so a worker cannot request a Git
/// mutation that anything would honour (B4-DESIGN §1.5).
///
/// The first call reconciles, which is a no-op on a clean store and the whole
/// resume protocol on a crashed one: every intent below is idempotent at the
/// repository level, so replaying this function after a crash reaches the same
/// receipts with zero duplicate branches, worktrees, pushes, or pull requests.
///
/// This seam is reached only behind the fixture-backed run path, and after B5
/// that is a statement about authority rather than about a flag: `seal`'s Git
/// half exists only when the enrollment carried a fixture-minted
/// `GitEffectCapability`, and a run sealed with `None` — every run the shipped
/// binary performs — resolves to `ExecutionEnrollment::Unenrolled` and returns
/// `CliError::ExecutionNotEnrolled` before any phase is dispatched.
///
/// # Errors
///
/// Returns [`GitRunError`] when dispatch fails, when a governed Git effect is
/// refused, or when a completed refresh yields no base.
pub fn run_one_phase_with_governed_git(
    mission_id: &str,
    domain: &str,
    phase: &ResolvedPhase,
    registry: &ExecutorRegistry,
    context: &mut ExecutionContext<'_>,
    resume_from: Option<SessionHandle>,
    governed: GovernedGit<'_, '_>,
) -> Result<GitRunReceipts, GitRunError> {
    let GovernedGit { service: git, plan } = governed;
    let _reconciled = git.reconcile()?;

    let base = git.apply(GitIntent::RefreshBase {
        remote: plan.remote.clone(),
        base_branch: plan.base_branch.clone(),
    })?;
    let refreshed = git
        .take_refreshed_base()
        .ok_or(GitRunError::NoRefreshedBase)?;
    let mission =
        orchestrator_core::MissionId::new(mission_id).map_err(|_| GitRunError::InvalidMissionId)?;
    let isolation = git.apply(GitIntent::CreateIsolation {
        base: refreshed,
        mission_id: mission,
        task: plan.task.clone(),
    })?;

    let outcome = run_one_phase(mission_id, domain, phase, registry, context, resume_from)?;

    let mut receipts = GitRunReceipts {
        base,
        isolation,
        outcome,
        commit: None,
        push: None,
        pull_request: None,
        cleanup: None,
    };

    // Unverified work is never committed, and because nothing was committed
    // there is nothing to publish either.
    if let Some(work) =
        orchestrator_app::VerifiedWork::from_outcome(&phase.id, 1, &receipts.outcome)
    {
        receipts.commit = Some(git.apply(GitIntent::Commit {
            message: plan.commit_message.clone(),
            work,
        })?);
        let branch = git
            .isolation_branch()
            .ok_or(GitEffectError::MissingIsolation)?
            .to_owned();
        let push = git.apply(GitIntent::Push {
            remote: plan.remote.clone(),
            branch,
        })?;
        // §4.3: an acknowledgement is two facts, not one. Anything short of
        // `Acknowledged` stops the run with the push row retained rather than
        // opening a pull request for a commit the remote may not hold.
        let acknowledged = matches!(
            push,
            GitReceipt::Pushed {
                ack: PushAck::Acknowledged { .. },
                ..
            }
        );
        receipts.push = Some(push);
        if acknowledged {
            receipts.pull_request = Some(git.apply(GitIntent::OpenPr {
                title: plan.pr_title.clone(),
                body: plan.pr_body.clone(),
                draft: plan.pr_draft,
            })?);
        }
    }

    receipts.cleanup = Some(git.apply(GitIntent::Cleanup {
        trash: TrashRoot::under(git.capability(), &plan.trash_component)?,
    })?);
    Ok(receipts)
}

/// Builds a validated [`ExecutionRequest`] for one resolved phase.
///
/// `runtime` is the registry's effective runtime so the request passes the
/// registry's runtime-match check. An empty workdir is normalized to the current
/// directory so the request's path validation accepts it. `resume_from` is
/// threaded straight into the draft; construction fail-closes on a
/// cross-family handle before any dispatch is attempted.
fn build_execution_request(
    mission_id: &str,
    domain: &str,
    phase: &ResolvedPhase,
    runtime: RuntimeFamily,
    resume_from: Option<SessionHandle>,
) -> Result<ExecutionRequest, ContractError> {
    let effort = match phase.effort.as_str() {
        "low" => Effort::Low,
        "medium" => Effort::Medium,
        "xhigh" => Effort::XHigh,
        _ => Effort::High,
    };
    let worker_dir = if phase.workdir.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(&phase.workdir)
    };
    ExecutionRequest::new(ExecutionRequestDraft {
        mission: mission_id.to_owned(),
        phase: phase.id.to_string(),
        attempt: 1,
        revision: 1,
        objective: phase.objective.clone(),
        persona: phase.persona.clone(),
        role: phase.role.clone(),
        domain: domain.to_owned(),
        skills: phase.skills.clone(),
        dependencies: phase.dependencies.iter().map(|id| id.to_string()).collect(),
        expected_evidence: Vec::new(),
        constraints: Vec::new(),
        prior_context: String::new(),
        runtime,
        model: phase.model.clone(),
        effort,
        max_turns: 0,
        worker_dir,
        target_dir: None,
        resume_from,
        hook_script: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn args(slice: &[&str]) -> Vec<String> {
        slice.iter().map(|&s| s.to_owned()).collect()
    }

    #[test]
    fn parses_run_value_flags_with_space_and_equals_forms() -> TestResult {
        let (flags, positional) = parse(&args(&[
            "--runtime",
            "codex",
            "--gate-mode=warn",
            "--stall-timeout",
            "10m",
            "fix",
            "the",
            "bug",
        ]))?;
        assert_eq!(flags.runtime.as_deref(), Some("codex"));
        assert_eq!(flags.gate_mode, "warn");
        assert_eq!(flags.stall_timeout.as_deref(), Some("10m"));
        assert_eq!(
            positional,
            ["fix".to_string(), "the".to_string(), "bug".to_string()]
        );
        Ok(())
    }

    #[test]
    fn bool_flags_default_and_override() -> TestResult {
        let (flags, _) = parse(&args(&["--pr", "--git-isolate=false"]))?;
        assert!(flags.pr);
        assert!(!flags.git_isolate);
        let (defaults, _) = parse(&args(&[]))?;
        assert!(defaults.git_isolate);
        assert!(!defaults.pr);
        assert_eq!(defaults.gate_mode, "block");
        Ok(())
    }

    #[test]
    fn persistent_flags_round_trip() -> TestResult {
        let (flags, _) = parse(&args(&[
            "--verbose",
            "--domain",
            "work",
            "--model=opus",
            "--max-turns",
            "30",
            "--personas-dir",
            "/p",
            "task",
        ]))?;
        assert!(flags.persistent.verbose);
        assert_eq!(flags.persistent.domain, "work");
        assert_eq!(flags.persistent.model, "opus");
        assert_eq!(flags.persistent.max_turns, Some(30));
        assert_eq!(
            flags.persistent.personas_dir.as_deref(),
            Some(Path::new("/p"))
        );
        Ok(())
    }

    #[test]
    fn nanika_directory_precedence_matches_the_compatibility_contract() -> TestResult {
        let context = RunResolutionContext {
            environment: EnvInputs {
                nanika_dir: Some(PathBuf::from("/environment/nanika")),
                legacy_via_dir: Some(PathBuf::from("/legacy/via")),
                ..EnvInputs::default()
            },
            user_home: Some(PathBuf::from("/fixture/home")),
            ..RunResolutionContext::default()
        };

        let from_flag =
            resolve_with_context(&args(&["--nanika-dir", "/flag/nanika", "task"]), &context)?;
        assert_eq!(from_flag.nanika_dir, PathBuf::from("/flag/nanika"));

        let from_environment = resolve_with_context(&args(&["task"]), &context)?;
        assert_eq!(
            from_environment.nanika_dir,
            PathBuf::from("/environment/nanika")
        );

        let mut legacy_context = context.clone();
        legacy_context.environment.nanika_dir = None;
        let from_legacy = resolve_with_context(&args(&["task"]), &legacy_context)?;
        assert_eq!(from_legacy.nanika_dir, PathBuf::from("/legacy/via"));

        legacy_context.environment.legacy_via_dir = None;
        let from_default = resolve_with_context(&args(&["task"]), &legacy_context)?;
        assert_eq!(
            from_default.nanika_dir,
            PathBuf::from("/fixture/home/nanika")
        );

        let empty_flag = resolve_with_context(&args(&["--nanika-dir=", "task"]), &context)?;
        assert_eq!(empty_flag.nanika_dir, PathBuf::from("/environment/nanika"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_nanika_environment_path_survives_resolution() -> TestResult {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let raw = OsString::from_vec(b"/fixture/nanika-\xff".to_vec());
        let path = PathBuf::from(raw);
        let context = RunResolutionContext {
            environment: EnvInputs {
                nanika_dir: Some(path.clone()),
                ..EnvInputs::default()
            },
            user_home: Some(PathBuf::from("/fixture/home")),
            ..RunResolutionContext::default()
        };

        let resolved = resolve_with_context(&args(&["task"]), &context)?;
        assert_eq!(resolved.nanika_dir, path);
        Ok(())
    }

    #[test]
    fn verbose_short_equals_form_matches_cobra_boolean_syntax() -> TestResult {
        let (disabled, _) = parse(&args(&["-v=false", "task"]))?;
        let (enabled, _) = parse(&args(&["-v=true", "task"]))?;
        assert!(!disabled.persistent.verbose);
        assert!(enabled.persistent.verbose);
        Ok(())
    }

    #[test]
    fn go_boolean_spellings_match_strconv_parse_bool() -> TestResult {
        for (value, expected) in [
            ("1", true),
            ("t", true),
            ("T", true),
            ("TRUE", true),
            ("true", true),
            ("True", true),
            ("0", false),
            ("f", false),
            ("F", false),
            ("FALSE", false),
            ("false", false),
            ("False", false),
        ] {
            assert_eq!(parse_bool("fixture", Some(value))?, expected, "{value}");
        }
        Ok(())
    }

    #[test]
    fn double_dash_treats_rest_as_positional() -> TestResult {
        let (flags, positional) = parse(&args(&["--", "--not-a-flag", "task"]))?;
        assert!(!flags.pr);
        assert_eq!(positional, ["--not-a-flag".to_string(), "task".to_string()]);
        Ok(())
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(matches!(
            parse(&args(&["--nonsense"])),
            Err(RunResolveError::UnknownFlag(name)) if name == "nonsense"
        ));
    }

    #[test]
    fn missing_value_is_rejected() {
        assert!(matches!(
            parse(&args(&["--runtime"])),
            Err(RunResolveError::FlagNeedsValue { flag }) if flag == "runtime"
        ));
    }

    #[test]
    fn invalid_bool_is_rejected() {
        assert!(matches!(
            parse(&args(&["--pr=maybe"])),
            Err(RunResolveError::InvalidBool { flag, .. }) if flag == "pr"
        ));
    }

    #[test]
    fn pr_with_no_git_is_rejected() {
        assert!(matches!(
            resolve(&args(&["--pr", "--no-git", "task"])),
            Err(RunResolveError::PrRequiresGitIsolation)
        ));
    }

    #[test]
    fn pr_with_git_isolate_false_is_rejected() {
        assert!(matches!(
            resolve(&args(&["--pr", "--git-isolate=false", "task"])),
            Err(RunResolveError::PrRequiresGitIsolation)
        ));
    }

    #[test]
    fn codex_review_requires_pr() {
        assert!(matches!(
            resolve(&args(&["--codex-review", "task"])),
            Err(RunResolveError::CodexReviewRequiresPr)
        ));
    }

    #[test]
    fn missing_task_without_resume_or_template_is_rejected() {
        assert!(matches!(
            resolve(&args(&[])),
            Err(RunResolveError::MissingTask)
        ));
    }

    #[test]
    fn resume_and_template_are_deferred() {
        assert!(matches!(
            resolve(&args(&["--resume", "/tmp/ws"])),
            Err(RunResolveError::NotYetImplemented("resume"))
        ));
        assert!(matches!(
            resolve(&args(&["--template", "t"])),
            Err(RunResolveError::NotYetImplemented("template"))
        ));
    }

    #[test]
    fn single_worker_task_has_no_plan() -> TestResult {
        let resolved = resolve_with(&args(&["fix the bug"]), &EnvInputs::default())?;
        assert_eq!(resolved.task, "fix the bug");
        assert!(resolved.phases.is_empty());
        assert!(resolved.mission_path.is_none());
        Ok(())
    }

    #[test]
    fn predecomposed_mission_text_plans_phases() -> TestResult {
        let mission = "PHASE: plan | OBJECTIVE: sketch design | PERSONA: architect\n\
                       PHASE: build | OBJECTIVE: implement it | DEPENDS: plan\n";
        let resolved = resolve_with(&args(&[mission]), &EnvInputs::default())?;
        assert_eq!(resolved.phases.len(), 2);
        assert_eq!(resolved.phases[0].name, "plan");
        assert_eq!(resolved.phases[1].name, "build");
        // With an empty persona catalog the authored PERSONA falls back to the
        // default policy persona (generalist), tier work, policy runtime claude.
        assert_eq!(resolved.phases[0].persona, "generalist");
        assert_eq!(resolved.phases[0].effective_runtime, "claude");
        assert_eq!(resolved.phases[0].model, "sonnet");
        assert_eq!(resolved.phases[0].effort, "medium");
        Ok(())
    }

    #[test]
    fn sequential_flag_is_retained_without_relabeling_the_authored_plan() -> TestResult {
        let mission = "PHASE: plan | OBJECTIVE: sketch design\n\
                       PHASE: build | OBJECTIVE: implement it\n";
        let resolved = resolve_with(
            &args(&["--dry-run", "--sequential", mission]),
            &EnvInputs::default(),
        )?;
        let mut output = Vec::new();
        render(&resolved, false, &mut output)?;
        let output = String::from_utf8(output)?;

        assert!(resolved.flags.persistent.sequential);
        assert_eq!(resolved.execution_mode, ExecutionMode::Parallel);
        assert!(output.contains("phases: 2 (parallel)"));
        Ok(())
    }

    #[test]
    fn authored_phase_fields_survive_resolution() -> TestResult {
        let mission = "PHASE: plan | OBJECTIVE: sketch design | SKILLS: rust-best-practices,decomposer | EXPECTED: design.md | WORKDIR: ~/nanika | TIMEOUT: 5m | PRIORITY: p0\n\
                       PHASE: build | OBJECTIVE: implement it | DEPENDS: plan | EXPECTED: passing tests\n";
        let context = RunResolutionContext {
            user_home: Some(PathBuf::from("/fixture/home")),
            ..RunResolutionContext::default()
        };
        let resolved = resolve_with_context(&args(&[mission]), &context)?;
        let plan = &resolved.phases[0];
        assert_eq!(plan.objective, "sketch design");
        assert_eq!(plan.skills, ["rust-best-practices", "decomposer"]);
        assert_eq!(plan.expected, "design.md");
        assert_eq!(plan.workdir, "/fixture/home/nanika");
        assert_eq!(plan.authored_model_tier, "work");
        assert_eq!(plan.authored_stall_timeout, Some(Duration::from_secs(300)));
        assert_eq!(plan.stall_timeout, Some(Duration::from_secs(300)));
        assert_eq!(plan.stall_timeout_source, StallSource::Phase);
        assert_eq!(plan.priority, "P0");
        assert_eq!(
            resolved.phases[1].dependencies.as_slice(),
            std::slice::from_ref(&plan.id)
        );
        Ok(())
    }

    #[test]
    fn malformed_authored_phase_is_not_silently_downgraded_to_single_worker() {
        let result = resolve_with(
            &args(&["PHASE: broken | PERSONA: architect"]),
            &EnvInputs::default(),
        );
        assert!(matches!(
            result,
            Err(RunResolveError::AuthoredMission(
                MissionParseError::NoPhases
            ))
        ));
    }

    #[test]
    fn composition_context_supplies_personas_and_routing() -> TestResult {
        let mut personas = PersonaCatalog::default();
        personas.names.insert("architect".to_owned());
        let mut routing = RoutingMap::default();
        routing.model_tiers.insert(
            "work".to_owned(),
            orchestrator_core::RoutingTier {
                provider: "openai".to_owned(),
                model: "configured-model".to_owned(),
                runtime: "codex".to_owned(),
            },
        );
        let context = RunResolutionContext {
            environment: EnvInputs {
                default_runtime: Some("codex".to_owned()),
                codex_auto: String::new(),
                stall_timeout: None,
                ..EnvInputs::default()
            },
            personas,
            routing,
            ..RunResolutionContext::default()
        };
        let mission = "PHASE: plan | OBJECTIVE: sketch design | PERSONA: architect\n";
        let resolved = resolve_with_context(&args(&[mission]), &context)?;
        assert_eq!(resolved.phases[0].persona, "architect");
        assert_eq!(resolved.phases[0].effective_runtime, "codex");
        assert_eq!(resolved.phases[0].model, "configured-model");
        Ok(())
    }

    #[test]
    fn authored_codex_runtime_is_honored() -> TestResult {
        let mission = "PHASE: port | OBJECTIVE: implement the parser | RUNTIME: codex\n";
        let resolved = resolve_with(&args(&[mission]), &EnvInputs::default())?;
        let phase = &resolved.phases[0];
        assert_eq!(phase.authored_runtime, "codex");
        assert!(!phase.runtime_policy_applied);
        assert_eq!(phase.effective_runtime, "codex");
        // Work-tier codex resolves to gpt-5.6-sol at medium effort per Go's
        // ResolveEffortForRuntime (default arm); the prior glm-5.2/high values
        // encoded the pre-catch-up router mis-port.
        assert_eq!(phase.model, "gpt-5.6-sol");
        assert_eq!(phase.effort, "medium");
        Ok(())
    }

    #[test]
    fn forced_runtime_overrides_policy() -> TestResult {
        let mission = "PHASE: port | OBJECTIVE: implement the parser\n";
        let resolved = resolve_with(
            &args(&["--runtime", "codex", mission]),
            &EnvInputs::default(),
        )?;
        // policy-applied phase (no authored RUNTIME) honors --runtime.
        assert_eq!(resolved.phases[0].authored_runtime, "claude");
        assert!(resolved.phases[0].runtime_policy_applied);
        assert_eq!(resolved.phases[0].effective_runtime, "codex");
        Ok(())
    }

    #[test]
    fn authored_stall_timeout_overrides_flag_and_environment() -> TestResult {
        let mission = "PHASE: port | OBJECTIVE: implement | TIMEOUT: 5m\n";
        let context = RunResolutionContext {
            environment: EnvInputs {
                stall_timeout: Some("30m".to_owned()),
                ..EnvInputs::default()
            },
            ..RunResolutionContext::default()
        };
        let resolved = resolve_with_context(&args(&["--stall-timeout", "20m", mission]), &context)?;
        assert_eq!(
            resolved.phases[0].stall_timeout,
            Some(Duration::from_secs(300))
        );
        assert_eq!(resolved.phases[0].stall_timeout_source, StallSource::Phase);
        Ok(())
    }

    #[test]
    fn stall_flag_overrides_environment() -> TestResult {
        let mission = "PHASE: port | OBJECTIVE: implement\n";
        let context = RunResolutionContext {
            environment: EnvInputs {
                stall_timeout: Some("30m".to_owned()),
                ..EnvInputs::default()
            },
            ..RunResolutionContext::default()
        };
        let resolved = resolve_with_context(&args(&["--stall-timeout", "20m", mission]), &context)?;
        assert_eq!(
            resolved.phases[0].stall_timeout,
            Some(Duration::from_secs(1_200))
        );
        assert_eq!(resolved.phases[0].stall_timeout_source, StallSource::Flag);
        Ok(())
    }

    #[test]
    fn stall_environment_applies_without_phase_or_flag() -> TestResult {
        let mission = "PHASE: port | OBJECTIVE: implement\n";
        let context = RunResolutionContext {
            environment: EnvInputs {
                stall_timeout: Some("30m".to_owned()),
                ..EnvInputs::default()
            },
            ..RunResolutionContext::default()
        };
        let resolved = resolve_with_context(&args(&[mission]), &context)?;
        assert_eq!(
            resolved.phases[0].stall_timeout,
            Some(Duration::from_secs(1_800))
        );
        assert_eq!(
            resolved.phases[0].stall_timeout_source,
            StallSource::Environment
        );
        Ok(())
    }

    #[test]
    fn stall_worker_default_remains_explicit_without_configuration() -> TestResult {
        let resolved = resolve_with(&args(&["single worker task"]), &EnvInputs::default())?;
        assert_eq!(
            resolved.default_stall_timeout,
            StallTimeoutResolution {
                value: StallTimeoutValue::WorkerDefault,
                source: StallSource::WorkerDefault,
            }
        );
        Ok(())
    }

    #[test]
    fn invalid_stall_flag_is_rejected_before_execution() {
        assert!(matches!(
            resolve_with(
                &args(&["--stall-timeout", "0s", "task"]),
                &EnvInputs::default()
            ),
            Err(RunResolveError::Configuration(
                ConfigError::InvalidStallFlag { value }
            )) if value == "0s"
        ));
    }

    #[test]
    fn invalid_stall_environment_is_rejected_before_execution() {
        let environment = EnvInputs {
            stall_timeout: Some("not-a-duration".to_owned()),
            ..EnvInputs::default()
        };
        assert!(matches!(
            resolve_with(&args(&["task"]), &environment),
            Err(RunResolveError::Configuration(
                ConfigError::InvalidStallEnvironment { value }
            )) if value == "not-a-duration"
        ));
    }

    #[test]
    fn mission_file_is_read_when_task_names_a_markdown_file() -> TestResult {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "orchestrator-rs-run-mission-{}-{}.md",
            std::process::id(),
            CASE_NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(
            &path,
            "PHASE: a | OBJECTIVE: do a\nPHASE: b | OBJECTIVE: do b\n",
        )?;
        let path_str = path.to_str().ok_or("non-utf8 temp path")?;
        let resolved = resolve_with(&args(&[path_str]), &EnvInputs::default())?;
        assert!(resolved.mission_path.is_some());
        assert_eq!(resolved.phases.len(), 2);
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    use std::sync::atomic::AtomicU64;
    static CASE_NEXT: AtomicU64 = AtomicU64::new(1);
}
