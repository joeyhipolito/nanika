//! Non-enrolled, one-shot Claude providers for the Rust orchestrator.
//!
//! This crate translates a registry-bound phase into the reviewed Claude CLI
//! wire contract. It never resolves or starts an operating-system process. The
//! only operation-performing boundary is [`ExecutionContext::run_process`], so
//! enrollment, process-tree ownership, cancellation, deadlines, and output
//! capture remain the composition root's responsibility.
//!
//! Production enrollment is intentionally blocked. Review remains tool-less.
//! The feature-gated first-use coding executor is a separate restricted
//! file-tools contract and cannot be constructed by the ordinary CLI. The exact 2.1.211 command
//! surface now requests safe mode and disables every optional extension surface,
//! but admin-managed policy still applies and hooks may execute before the
//! first observable `system/init` record. A disposable trap proof must establish
//! subscription-safe launch isolation before enrollment. See
//! [`ClaudeProvider::enrollment_status`].

mod statusline_usage;

#[cfg(any(test, feature = "experimental-first-use-pilot"))]
mod coding;

#[cfg(feature = "experimental-first-use-pilot")]
pub use coding::{ClaudeCodingExecutor, FIRST_USE_PILOT_CLAUDE_MAX_TURNS};

pub use statusline_usage::{
    CLAUDE_STATUSLINE_SOURCE_ID, ClaudeStatuslineQuotaObservationV1, ClaudeStatuslineQuotaWindowV1,
    ClaudeStatuslineUsageError, MAX_CLAUDE_STATUSLINE_INPUT_BYTES,
    SUPPORTED_CLAUDE_STATUSLINE_VERSION, decode_claude_statusline_usage,
};

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::Duration;

use orchestrator_exec::{
    AttemptEvidence, AttemptOutcome, ContractError, CostInfo, DispatchRequest, ExecutionContext,
    ExecutionRequest, Failure, FailureKind, MechanicalTermination, PartialWork, PhaseExecutor,
    ProcessPurpose, ProcessReceipt, ProcessRequest, ProcessTerminationReceipt, RuntimeCaps,
    RuntimeDescriptor, RuntimeFamily,
};
use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use thiserror::Error;

const CLAUDE_RUNTIME: &str = "claude";
const CLAUDE_EXECUTABLE_ID: &str = "claude";
const EMPTY_MCP_CONFIG: &str = r#"{"mcpServers":{}}"#;
const ENTRYPOINT_KEY: &str = "CLAUDE_CODE_ENTRYPOINT";
const ENTRYPOINT_VALUE: &str = "orchestrator-cli";
const MAX_ARGUMENT_BYTES: usize = 8 * 1024;
const MAX_ENVIRONMENT_VALUE_BYTES: usize = 64 * 1024;
const MAX_PERSONA_NAME_BYTES: usize = 256;
const MAX_PRIOR_CONTEXT_BYTES: usize = 8_000;
const MAX_STDOUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_JSONL_LINE_BYTES: usize = 10 * 1024 * 1024;
const MAX_FINAL_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// Exact Claude Code build admitted by this protocol slice.
pub const PINNED_CLAUDE_CODE_VERSION: &str = "2.1.211";

/// Claude Code build admitted only by the explicit, non-enrolled first-use
/// pilot contract ([`ClaudeProvider::new_first_use_pilot`]). It does not relax
/// [`PINNED_CLAUDE_CODE_VERSION`]; the production contract still requires the
/// pinned build, and no executable digest is attested for this version.
#[cfg(any(test, feature = "experimental-first-use-pilot"))]
pub const FIRST_USE_PILOT_CLAUDE_CODE_VERSION: &str = "2.1.269";

/// Byte length of the reviewed 2.1.211 executable image.
pub const PINNED_CLAUDE_CODE_LENGTH_BYTES: u64 = 242_445_680;

/// SHA-256 of the reviewed, signed local 2.1.211 executable image.
pub const PINNED_CLAUDE_CODE_SHA256: [u8; 32] = [
    0x5a, 0x72, 0x8a, 0x76, 0x19, 0x8b, 0x6e, 0xca, 0x7f, 0x3c, 0x7c, 0xdb, 0xff, 0x43, 0xba, 0xb4,
    0x4b, 0x77, 0xb4, 0x8c, 0x21, 0x08, 0xf7, 0xa3, 0x10, 0x7d, 0x88, 0x97, 0x73, 0x38, 0x26, 0x29,
];

const PROMPT_PREAMBLE: &str =
    "You will be given source material from prior phase output below, followed by a task.\n\n";
const PRIOR_CONTEXT_OPEN: &str = "<prior_phase_output>\n";
const PRIOR_CONTEXT_CLOSE: &str = "\n</prior_phase_output>\n\n";
const PRIOR_CONTEXT_TRUNCATION_PREFIX: &str = "\n[Note: Prior context truncated; original was ";
const PRIOR_CONTEXT_TRUNCATION_SUFFIX: &str = " characters]";
const TASK_PREFIX: &str = "Task: ";

/// Fail-closed configuration errors. Values are deliberately absent from the
/// variants so ordinary `Display` and `Debug` cannot disclose credentials or
/// persona instructions.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ClaudeProviderConfigError {
    #[error("Claude environment key is not in the reviewed allowlist")]
    EnvironmentKeyNotAllowed,
    #[error("Claude environment key is configured more than once")]
    DuplicateEnvironmentKey,
    #[error("Claude environment value exceeds the process contract")]
    EnvironmentValueTooLong,
    #[error("Claude environment value contains a NUL byte")]
    EnvironmentValueContainsNul,
    #[error("Claude persona name must not be empty")]
    EmptyPersonaName,
    #[error("Claude system or persona prompt must not be empty")]
    EmptyPrompt,
    #[error("Claude persona name violates the bounded label contract")]
    InvalidPersonaName,
    #[error("Claude persona prompt is configured more than once")]
    DuplicatePersonaPrompt,
    #[error("Claude system prompt is configured more than once")]
    DuplicateSystemPrompt,
    #[error("Claude system or persona prompt exceeds the process contract")]
    PromptTooLong,
    #[error("Claude system or persona prompt contains a NUL byte")]
    PromptContainsNul,
}

/// Explicit, deterministic provider inputs. There is intentionally no
/// ambient-environment or passthrough switch.
#[derive(Clone, Default)]
pub struct ClaudeProviderConfig {
    environment: BTreeMap<String, String>,
    persona_prompts: BTreeMap<String, String>,
    system_prompt: Option<String>,
}

impl ClaudeProviderConfig {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one explicit value from the Go SDK's reviewed base allowlist.
    pub fn with_environment(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, ClaudeProviderConfigError> {
        let key = key.into();
        let value = value.into();
        if !is_allowed_environment_key(&key) {
            return Err(ClaudeProviderConfigError::EnvironmentKeyNotAllowed);
        }
        validate_environment_value(&value)?;
        if self.environment.insert(key, value).is_some() {
            return Err(ClaudeProviderConfigError::DuplicateEnvironmentKey);
        }
        Ok(self)
    }

    /// Configures a full system-prompt replacement for personas without an
    /// append prompt. A persona append always wins, matching `queryOptFlags`.
    pub fn with_system_prompt(
        mut self,
        prompt: impl Into<String>,
    ) -> Result<Self, ClaudeProviderConfigError> {
        let prompt = prompt.into();
        validate_prompt(&prompt)?;
        if self.system_prompt.is_some() {
            return Err(ClaudeProviderConfigError::DuplicateSystemPrompt);
        }
        self.system_prompt = Some(prompt);
        Ok(self)
    }

    /// Maps a validated request persona label to the full briefing appended to
    /// Claude's built-in system prompt.
    pub fn with_persona_prompt(
        mut self,
        persona: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Result<Self, ClaudeProviderConfigError> {
        let persona = persona.into();
        let prompt = prompt.into();
        if persona.is_empty() {
            return Err(ClaudeProviderConfigError::EmptyPersonaName);
        }
        if persona.len() > MAX_PERSONA_NAME_BYTES || persona.chars().any(char::is_control) {
            return Err(ClaudeProviderConfigError::InvalidPersonaName);
        }
        validate_prompt(&prompt)?;
        if self.persona_prompts.insert(persona, prompt).is_some() {
            return Err(ClaudeProviderConfigError::DuplicatePersonaPrompt);
        }
        Ok(self)
    }
}

impl fmt::Debug for ClaudeProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaudeProviderConfig")
            .field("environment_count", &self.environment.len())
            .field("persona_prompt_count", &self.persona_prompts.len())
            .field("has_system_prompt", &self.system_prompt.is_some())
            .finish()
    }
}

/// A production-shaped provider that remains inert until a composition root
/// enrolls `claude` in a concrete [`orchestrator_exec::ProcessService`].
pub struct ClaudeProvider {
    config: ClaudeProviderConfig,
    runtime: RuntimeFamily,
    contract: ClaudeProtocolContract,
}

/// The reviewed enrollment gate for this provider slice.
///
/// Safe-mode argv is necessary but not sufficient: admin-managed policy still
/// applies, and a hook may have side effects before its lifecycle record is
/// parsed. A descriptor therefore does not authorize a composition root to
/// enroll this executor until the pinned subscription-auth trap proof passes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ClaudeEnrollmentStatus {
    BlockedUntilSubscriptionIsolationTrapPasses,
}

impl ClaudeProvider {
    pub fn new(config: ClaudeProviderConfig) -> Result<Self, ContractError> {
        Ok(Self {
            config,
            runtime: RuntimeFamily::parse(CLAUDE_RUNTIME)?,
            contract: ClaudeProtocolContract::production(),
        })
    }

    /// Builds the provider for the explicit first-use pilot binary only.
    ///
    /// The argv surface is identical to [`Self::new`]. The required
    /// `system/init` version is [`FIRST_USE_PILOT_CLAUDE_CODE_VERSION`], and
    /// only the recorded 2.1.269 wire drift is validated and removed before
    /// the unchanged strict parser runs. Enrollment stays blocked.
    #[cfg(feature = "experimental-first-use-pilot")]
    pub fn new_first_use_pilot(config: ClaudeProviderConfig) -> Result<Self, ContractError> {
        Ok(Self {
            config,
            runtime: RuntimeFamily::parse(CLAUDE_RUNTIME)?,
            contract: ClaudeProtocolContract::first_use_pilot(),
        })
    }

    #[must_use]
    pub const fn enrollment_status(&self) -> ClaudeEnrollmentStatus {
        ClaudeEnrollmentStatus::BlockedUntilSubscriptionIsolationTrapPasses
    }

    fn process_request(
        &self,
        request: &ExecutionRequest,
    ) -> Result<ProcessRequest, ClaudeRequestError> {
        if request.target_dir().is_some() {
            return Err(ClaudeRequestError::TargetDirectoryNotIsolated);
        }
        let working_root = request.worker_dir();
        let prompt = build_worker_prompt(request.prior_context(), request.objective())?;
        let options = OneShotOptions {
            model: request.model(),
            max_turns: request.max_turns(),
            append_system_prompt: self
                .config
                .persona_prompts
                .get(request.persona())
                .map(String::as_str),
            system_prompt: self.config.system_prompt.as_deref(),
            effort: request.effort().as_str(),
        };

        let mut process = ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            CLAUDE_EXECUTABLE_ID,
            working_root,
        )
        .map_err(|_| ClaudeRequestError::ProcessContract)?;
        for argument in build_isolated_one_shot_arguments(&options) {
            process = process
                .with_argument(argument)
                .map_err(|_| ClaudeRequestError::ProcessContract)?;
        }
        process = process
            .with_argument("-p")
            .and_then(|request| request.with_argument(prompt))
            .and_then(|request| request.with_max_output_bytes(MAX_STDOUT_BYTES))
            .map_err(|_| ClaudeRequestError::ProcessContract)?;

        for (key, value) in &self.config.environment {
            process = process
                .with_environment(key.clone(), value.clone())
                .map_err(|_| ClaudeRequestError::ProcessContract)?;
        }
        process = process
            .with_environment(ENTRYPOINT_KEY, ENTRYPOINT_VALUE)
            .map_err(|_| ClaudeRequestError::ProcessContract)?;
        Ok(process)
    }
}

impl fmt::Debug for ClaudeProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaudeProvider")
            .field("runtime", &self.runtime)
            .field("config", &self.config)
            .finish()
    }
}

impl PhaseExecutor for ClaudeProvider {
    fn execute(
        &self,
        dispatch: DispatchRequest<'_>,
        context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome {
        let process = match self.process_request(dispatch.request()) {
            Ok(process) => process,
            Err(_) => {
                return incomplete_with_failure(
                    MechanicalTermination::ContractViolation,
                    FailureKind::Protocol,
                    "Claude request could not satisfy the bounded process contract",
                    Duration::ZERO,
                );
            }
        };
        let receipt = match context.run_process(&process) {
            Ok(receipt) => receipt,
            Err(_) => {
                // ExecutionContext retains the authoritative, redacted service
                // failure. Do not duplicate or expose its detail here.
                return AttemptOutcome::incomplete(
                    MechanicalTermination::SupervisorFailure,
                    None,
                    PartialWork::empty(),
                    Duration::ZERO,
                );
            }
        };
        if !receipt.is_success() {
            return AttemptOutcome::incomplete(
                mechanical_termination(&receipt),
                None,
                PartialWork::empty(),
                receipt.elapsed(),
            );
        }

        let parsed =
            match parse_one_shot_output_with_contract(receipt.expose_stdout(), self.contract) {
                Ok(parsed) => parsed,
                Err(error) => {
                    return incomplete_with_failure(
                        error.termination(),
                        error.failure_kind(),
                        error.failure_detail(),
                        receipt.elapsed(),
                    );
                }
            };
        let evidence = parsed.cost.map_or_else(AttemptEvidence::new, |cost| {
            AttemptEvidence::new().with_cost(cost)
        });
        match AttemptOutcome::completed(parsed.output, evidence, receipt.elapsed()) {
            Ok(outcome) => outcome,
            Err(_) => incomplete_with_failure(
                MechanicalTermination::ContractViolation,
                FailureKind::Protocol,
                "Claude output violated the completed-attempt contract",
                receipt.elapsed(),
            ),
        }
    }

    fn descriptor(&self) -> Option<RuntimeDescriptor> {
        Some(RuntimeDescriptor::new(
            self.runtime.clone(),
            RuntimeCaps {
                tool_use: false,
                session_resume: false,
                streaming: false,
                cost_report: true,
                artifacts: false,
            },
        ))
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
enum ClaudeRequestError {
    #[error("Claude target-directory access is not admitted by the isolated provider")]
    TargetDirectoryNotIsolated,
    #[error("Claude task cannot fit the authority-bounded process argument")]
    TaskExceedsArgumentBudget,
    #[error("Claude process request violates a bounded contract")]
    ProcessContract,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
enum ClaudeOutputError {
    #[error("Claude output exceeded the cumulative byte limit")]
    CumulativeLimit,
    #[error("Claude output contained an oversized JSONL record")]
    LineLimit,
    #[error("Claude output contained malformed or ambiguous JSON")]
    Malformed,
    #[error("Claude output contained an unknown message or content type")]
    UnknownProtocolValue,
    #[error("Claude emitted a tool protocol message in tool-less mode")]
    ToolProtocol,
    #[error("Claude coding emitted a forbidden tool, network, or subagent record")]
    ForbiddenCodingActivity,
    #[error(
        "Claude coding attempted a path outside the isolated workspace or a protected configuration path"
    )]
    ToolPath,
    #[error("Claude coding encountered a denied or failed file tool")]
    PermissionDenied,
    #[error("Claude coding ended without a successful file mutation")]
    MissingMutation,
    #[error("Claude runtime reported an unisolated extension surface")]
    UnisolatedRuntime,
    #[error("Claude reported an unsuccessful result")]
    ProviderReportedError,
    #[error("Claude output ended without a successful result")]
    MissingResult,
    #[error("Claude returned no authoritative text")]
    EmptyOutput,
    #[error("Claude authoritative text exceeded the attempt output limit")]
    FinalOutputLimit,
    #[error("Claude cost telemetry was invalid")]
    InvalidCost,
}

impl ClaudeOutputError {
    const fn termination(self) -> MechanicalTermination {
        match self {
            Self::ProviderReportedError | Self::MissingResult | Self::EmptyOutput => {
                MechanicalTermination::ProviderStreamEnded
            }
            Self::CumulativeLimit
            | Self::LineLimit
            | Self::Malformed
            | Self::UnknownProtocolValue
            | Self::ToolProtocol
            | Self::ForbiddenCodingActivity
            | Self::ToolPath
            | Self::PermissionDenied
            | Self::MissingMutation
            | Self::UnisolatedRuntime
            | Self::FinalOutputLimit
            | Self::InvalidCost => MechanicalTermination::ContractViolation,
        }
    }

    const fn failure_kind(self) -> FailureKind {
        match self {
            Self::Malformed => FailureKind::Parse,
            Self::ProviderReportedError => FailureKind::Transport,
            Self::EmptyOutput => FailureKind::Semantic,
            Self::CumulativeLimit
            | Self::LineLimit
            | Self::UnknownProtocolValue
            | Self::ToolProtocol
            | Self::ForbiddenCodingActivity
            | Self::ToolPath
            | Self::PermissionDenied
            | Self::MissingMutation
            | Self::UnisolatedRuntime
            | Self::MissingResult
            | Self::FinalOutputLimit
            | Self::InvalidCost => FailureKind::Protocol,
        }
    }

    const fn failure_detail(self) -> &'static str {
        match self {
            Self::CumulativeLimit => "Claude stdout exceeded the cumulative byte limit",
            Self::LineLimit => "Claude stdout contained an oversized JSONL record",
            Self::Malformed => "Claude stdout contained malformed or ambiguous JSON",
            Self::UnknownProtocolValue => "Claude stdout contained an unknown protocol value",
            Self::ToolProtocol => "Claude emitted a tool message while tools were disabled",
            Self::ForbiddenCodingActivity => {
                "Claude coding emitted forbidden tool, network, or subagent activity"
            }
            Self::ToolPath => {
                "Claude file tool named a path outside the workspace or protected configuration"
            }
            Self::PermissionDenied => "Claude file-tool execution was denied or failed",
            Self::MissingMutation => "Claude reported success without a successful file mutation",
            Self::UnisolatedRuntime => {
                "Claude runtime reported an unisolated tool, hook, plugin, or extension surface"
            }
            Self::ProviderReportedError => "Claude reported an unsuccessful result",
            Self::MissingResult => "Claude stdout ended without a successful result",
            Self::EmptyOutput => "Claude returned no authoritative assistant text",
            Self::FinalOutputLimit => "Claude authoritative text exceeded the output limit",
            Self::InvalidCost => "Claude returned invalid cost telemetry",
        }
    }
}

struct OneShotOptions<'a> {
    model: &'a str,
    max_turns: u64,
    append_system_prompt: Option<&'a str>,
    system_prompt: Option<&'a str>,
    effort: &'a str,
}

#[cfg(test)]
struct GoOneShotOptions<'a> {
    model: &'a str,
    max_turns: u64,
    append_system_prompt: Option<&'a str>,
    system_prompt: Option<&'a str>,
    effort: &'a str,
    disable_builtin_tools: bool,
    disable_mcp: bool,
    add_directory: Option<&'a str>,
}

#[cfg(test)]
fn build_one_shot_arguments(options: &GoOneShotOptions<'_>) -> Vec<String> {
    let mut arguments = [
        "--output-format",
        "stream-json",
        "--print",
        "--verbose",
        "--include-partial-messages",
        "--dangerously-skip-permissions",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    if !options.model.is_empty() {
        arguments.extend(["--model".to_owned(), options.model.to_owned()]);
    }
    if options.max_turns > 0 {
        arguments.extend(["--max-turns".to_owned(), options.max_turns.to_string()]);
    }
    if let Some(prompt) = options
        .append_system_prompt
        .filter(|prompt| !prompt.is_empty())
    {
        arguments.extend(["--append-system-prompt".to_owned(), prompt.to_owned()]);
    } else if let Some(prompt) = options.system_prompt.filter(|prompt| !prompt.is_empty()) {
        arguments.extend(["--system-prompt".to_owned(), prompt.to_owned()]);
    }
    if !options.effort.is_empty() {
        arguments.extend(["--effort".to_owned(), options.effort.to_owned()]);
    }
    if options.disable_builtin_tools {
        arguments.extend(["--tools".to_owned(), String::new()]);
    }
    if options.disable_mcp {
        arguments.extend([
            "--strict-mcp-config".to_owned(),
            "--mcp-config".to_owned(),
            EMPTY_MCP_CONFIG.to_owned(),
        ]);
    }
    if let Some(directory) = options.add_directory {
        arguments.extend(["--add-dir".to_owned(), directory.to_owned()]);
    }
    arguments
}

/// Builds the Rust-only, non-enrolled isolation overlay for the pinned CLI.
///
/// This intentionally does not modify the frozen Go argv oracle above. Full
/// Go compatibility remains inspectable while every actual Rust provider
/// request uses the stricter command surface.
fn build_isolated_one_shot_arguments(options: &OneShotOptions<'_>) -> Vec<String> {
    let mut arguments = [
        "--output-format",
        "stream-json",
        "--input-format",
        "text",
        "--print",
        "--verbose",
        "--include-partial-messages",
        "--include-hook-events",
        "--safe-mode",
        "--setting-sources",
        "",
        "--disable-slash-commands",
        "--no-chrome",
        "--no-session-persistence",
        "--permission-mode",
        "dontAsk",
        "--prompt-suggestions",
        "false",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    append_model_prompt_effort(&mut arguments, options);
    arguments.extend(["--tools".to_owned(), String::new()]);
    arguments.extend([
        "--strict-mcp-config".to_owned(),
        "--mcp-config".to_owned(),
        EMPTY_MCP_CONFIG.to_owned(),
    ]);
    arguments
}

fn append_model_prompt_effort(arguments: &mut Vec<String>, options: &OneShotOptions<'_>) {
    if !options.model.is_empty() {
        arguments.extend(["--model".to_owned(), options.model.to_owned()]);
    }
    if options.max_turns > 0 {
        arguments.extend(["--max-turns".to_owned(), options.max_turns.to_string()]);
    }
    if let Some(prompt) = options
        .append_system_prompt
        .filter(|prompt| !prompt.is_empty())
    {
        arguments.extend(["--append-system-prompt".to_owned(), prompt.to_owned()]);
    } else if let Some(prompt) = options.system_prompt.filter(|prompt| !prompt.is_empty()) {
        arguments.extend(["--system-prompt".to_owned(), prompt.to_owned()]);
    }
    if !options.effort.is_empty() {
        arguments.extend(["--effort".to_owned(), options.effort.to_owned()]);
    }
}

fn build_worker_prompt(prior_context: &str, objective: &str) -> Result<String, ClaudeRequestError> {
    let task_len = TASK_PREFIX
        .len()
        .checked_add(objective.len())
        .ok_or(ClaudeRequestError::TaskExceedsArgumentBudget)?;
    if task_len > MAX_ARGUMENT_BYTES {
        return Err(ClaudeRequestError::TaskExceedsArgumentBudget);
    }

    let mut prompt = String::with_capacity(MAX_ARGUMENT_BYTES);
    let optional_budget = MAX_ARGUMENT_BYTES.saturating_sub(task_len);
    if optional_budget >= PROMPT_PREAMBLE.len() {
        prompt.push_str(PROMPT_PREAMBLE);
        append_bounded_prior_context(&mut prompt, prior_context, task_len);
    }
    prompt.push_str(TASK_PREFIX);
    prompt.push_str(objective);
    debug_assert!(prompt.len() <= MAX_ARGUMENT_BYTES);
    Ok(prompt)
}

fn append_bounded_prior_context(prompt: &mut String, prior_context: &str, task_len: usize) {
    if prior_context.is_empty() {
        return;
    }
    let original_len = prior_context.len();
    let truncation_note =
        format!("{PRIOR_CONTEXT_TRUNCATION_PREFIX}{original_len}{PRIOR_CONTEXT_TRUNCATION_SUFFIX}");
    let framing_len = PRIOR_CONTEXT_OPEN
        .len()
        .saturating_add(PRIOR_CONTEXT_CLOSE.len());
    let remaining = MAX_ARGUMENT_BYTES
        .saturating_sub(prompt.len())
        .saturating_sub(task_len);
    let full_context_len = prior_context.len().min(MAX_PRIOR_CONTEXT_BYTES);
    let full_fits = prior_context.len() <= MAX_PRIOR_CONTEXT_BYTES
        && framing_len.saturating_add(full_context_len) <= remaining;
    let note_len = if full_fits { 0 } else { truncation_note.len() };
    let content_budget = remaining.saturating_sub(framing_len.saturating_add(note_len));
    if content_budget == 0 {
        return;
    }

    let selected = truncate_utf8(prior_context, content_budget.min(MAX_PRIOR_CONTEXT_BYTES));
    if selected.is_empty() {
        return;
    }
    prompt.push_str(PRIOR_CONTEXT_OPEN);
    prompt.push_str(selected);
    if selected.len() < prior_context.len() {
        prompt.push_str(&truncation_note);
    }
    prompt.push_str(PRIOR_CONTEXT_CLOSE);
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    &value[..boundary]
}

fn is_allowed_environment_key(key: &str) -> bool {
    matches!(
        key,
        "HOME"
            | "PATH"
            | "LANG"
            | "TERM"
            | "USER"
            | "SHELL"
            | "TMPDIR"
            | "ANTHROPIC_BASE_URL"
            | "ANTHROPIC_API_KEY"
            | "ANTHROPIC_AUTH_TOKEN"
            | "CLAUDE_CONFIG_DIR"
    )
}

fn validate_environment_value(value: &str) -> Result<(), ClaudeProviderConfigError> {
    if value.len() > MAX_ENVIRONMENT_VALUE_BYTES {
        return Err(ClaudeProviderConfigError::EnvironmentValueTooLong);
    }
    if value.as_bytes().contains(&0) {
        return Err(ClaudeProviderConfigError::EnvironmentValueContainsNul);
    }
    Ok(())
}

fn validate_prompt(prompt: &str) -> Result<(), ClaudeProviderConfigError> {
    if prompt.is_empty() {
        return Err(ClaudeProviderConfigError::EmptyPrompt);
    }
    if prompt.len() > MAX_ARGUMENT_BYTES {
        return Err(ClaudeProviderConfigError::PromptTooLong);
    }
    if prompt.as_bytes().contains(&0) {
        return Err(ClaudeProviderConfigError::PromptContainsNul);
    }
    Ok(())
}

fn mechanical_termination(receipt: &ProcessReceipt) -> MechanicalTermination {
    match receipt.termination() {
        ProcessTerminationReceipt::Exited(status) => MechanicalTermination::ProcessExited(status),
        ProcessTerminationReceipt::Cancelled => MechanicalTermination::Cancelled,
        ProcessTerminationReceipt::DeadlineExceeded => MechanicalTermination::HardDeadlineExceeded,
        ProcessTerminationReceipt::Stalled => MechanicalTermination::WatchdogStalled,
        ProcessTerminationReceipt::OutputLimit
        | ProcessTerminationReceipt::SupervisorFailure
        | ProcessTerminationReceipt::UnresolvedOwnership => {
            MechanicalTermination::SupervisorFailure
        }
    }
}

fn incomplete_with_failure(
    termination: MechanicalTermination,
    kind: FailureKind,
    detail: &'static str,
    elapsed: Duration,
) -> AttemptOutcome {
    AttemptOutcome::incomplete(
        termination,
        Failure::new(kind, detail).ok(),
        PartialWork::empty(),
        elapsed,
    )
}

struct ParsedOutput {
    output: String,
    cost: Option<CostInfo>,
}

#[cfg(test)]
fn parse_one_shot_output(stdout: &[u8]) -> Result<ParsedOutput, ClaudeOutputError> {
    parse_one_shot_output_with_contract(stdout, ClaudeProtocolContract::production())
}

fn parse_one_shot_output_with_contract(
    stdout: &[u8],
    contract: ClaudeProtocolContract,
) -> Result<ParsedOutput, ClaudeOutputError> {
    let mut parser = OutputParser::new(contract);
    let mut start = 0;
    while start < stdout.len() {
        let remainder = &stdout[start..];
        let newline = remainder.iter().position(|byte| *byte == b'\n');
        let (mut line, consumed) = match newline {
            Some(offset) => (&remainder[..offset], offset.saturating_add(1)),
            None => (remainder, remainder.len()),
        };
        if line.last() == Some(&b'\r') {
            line = &line[..line.len().saturating_sub(1)];
        }
        parser.charge_line(line.len())?;
        if !line.is_empty() {
            parser.consume_line(line)?;
        }
        start = start.saturating_add(consumed);
    }
    parser.finish()
}

struct OutputParser {
    contract: ClaudeProtocolContract,
    saw_init: bool,
    charged_bytes: usize,
    output: String,
    cost: Option<CostInfo>,
    turn_state: TurnState,
}

#[derive(Clone, Copy)]
struct ClaudeProtocolContract {
    required_version: Option<&'static str>,
    required_permission_mode: Option<&'static str>,
    require_init: bool,
    /// Admits only the recorded Claude Code 2.1.269 drift, validated and
    /// removed by [`first_use_pilot_wire`] before the strict parser runs.
    #[cfg(any(test, feature = "experimental-first-use-pilot"))]
    first_use_pilot_wire: bool,
}

impl ClaudeProtocolContract {
    const fn production() -> Self {
        Self {
            required_version: Some(PINNED_CLAUDE_CODE_VERSION),
            required_permission_mode: Some("dontAsk"),
            require_init: true,
            #[cfg(any(test, feature = "experimental-first-use-pilot"))]
            first_use_pilot_wire: false,
        }
    }

    #[cfg(any(test, feature = "experimental-first-use-pilot"))]
    const fn first_use_pilot() -> Self {
        Self {
            required_version: Some(FIRST_USE_PILOT_CLAUDE_CODE_VERSION),
            required_permission_mode: Some("dontAsk"),
            require_init: true,
            first_use_pilot_wire: true,
        }
    }

    #[cfg(test)]
    const fn legacy_oracle() -> Self {
        Self {
            required_version: None,
            required_permission_mode: None,
            require_init: false,
            first_use_pilot_wire: false,
        }
    }
}

impl OutputParser {
    fn new(contract: ClaudeProtocolContract) -> Self {
        Self {
            contract,
            saw_init: false,
            charged_bytes: 0,
            output: String::new(),
            cost: None,
            turn_state: TurnState::AwaitingAuthoritativeText,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TurnState {
    AwaitingAuthoritativeText,
    EmptySuccessfulResult,
    AwaitingSuccessfulResult,
    SuccessfulResultAfterText,
}

impl OutputParser {
    fn charge_line(&mut self, line_bytes: usize) -> Result<(), ClaudeOutputError> {
        // bufio.Scanner with a 10 MiB maximum accepts content only while the
        // token plus its delimiter fits below that bound.
        if line_bytes >= MAX_JSONL_LINE_BYTES {
            return Err(ClaudeOutputError::LineLimit);
        }
        let charged = line_bytes
            .checked_add(1)
            .ok_or(ClaudeOutputError::CumulativeLimit)?;
        self.charged_bytes = self
            .charged_bytes
            .checked_add(charged)
            .ok_or(ClaudeOutputError::CumulativeLimit)?;
        if self.charged_bytes > MAX_STDOUT_BYTES {
            return Err(ClaudeOutputError::CumulativeLimit);
        }
        Ok(())
    }

    fn consume_line(&mut self, line: &[u8]) -> Result<(), ClaudeOutputError> {
        let json = std::str::from_utf8(line).map_err(|_| ClaudeOutputError::Malformed)?;
        reject_duplicate_object_keys(json)?;
        #[cfg(any(test, feature = "experimental-first-use-pilot"))]
        let normalized;
        #[cfg(any(test, feature = "experimental-first-use-pilot"))]
        let json = if self.contract.first_use_pilot_wire {
            match first_use_pilot_wire::normalize(json)? {
                first_use_pilot_wire::PilotLine::Drop => {
                    // No record may precede the isolated init, output or not.
                    if self.contract.require_init && !self.saw_init {
                        return Err(ClaudeOutputError::UnisolatedRuntime);
                    }
                    return Ok(());
                }
                first_use_pilot_wire::PilotLine::Strict(strict) => {
                    normalized = strict;
                    normalized.as_str()
                }
            }
        } else {
            json
        };
        let envelope: MessageEnvelope =
            serde_json::from_str(json).map_err(|_| ClaudeOutputError::Malformed)?;
        if self.contract.require_init
            && !self.saw_init
            && !matches!(envelope.message_type.as_str(), "system")
        {
            return Err(ClaudeOutputError::UnisolatedRuntime);
        }
        match envelope.message_type.as_str() {
            "assistant" => self.consume_assistant(json),
            "result" => self.consume_result(json),
            "system" => self.consume_system(json),
            // Partial stream events are validated even though this adapter does
            // not advertise streaming. Ignoring an unclassified nested value
            // would let tool protocol bypass the tool-less boundary.
            "stream_event" => self.consume_stream_event(json),
            "user" => Err(ClaudeOutputError::ToolProtocol),
            _ => Err(ClaudeOutputError::UnknownProtocolValue),
        }
    }

    fn consume_assistant(&mut self, json: &str) -> Result<(), ClaudeOutputError> {
        let assistant: AssistantWire =
            serde_json::from_str(json).map_err(|_| ClaudeOutputError::Malformed)?;
        let blocks = match &assistant {
            AssistantWire::Nested(nested) => {
                if nested.message_type != "assistant" {
                    return Err(ClaudeOutputError::Malformed);
                }
                let message = &nested.message;
                validate_assistant_message(message)?;
                message.content.as_slice()
            }
            AssistantWire::Direct(direct) => {
                if direct.message_type != "assistant" {
                    return Err(ClaudeOutputError::Malformed);
                }
                direct.content.as_slice()
            }
        };
        for block in blocks {
            match block {
                AssistantContentBlockWire::Text { text } => {
                    if !text.is_empty() {
                        self.push_output(text)?;
                        self.turn_state = TurnState::AwaitingSuccessfulResult;
                    }
                }
                AssistantContentBlockWire::Thinking { .. }
                | AssistantContentBlockWire::RedactedThinking { .. } => {}
                AssistantContentBlockWire::ToolUse { .. } => {
                    return Err(ClaudeOutputError::ToolProtocol);
                }
                AssistantContentBlockWire::Unknown => {
                    return Err(ClaudeOutputError::UnknownProtocolValue);
                }
            }
        }
        Ok(())
    }

    fn consume_result(&mut self, json: &str) -> Result<(), ClaudeOutputError> {
        let result: ResultWire =
            serde_json::from_str(json).map_err(|_| ClaudeOutputError::Malformed)?;
        let cost = result.into_success_cost()?;
        self.turn_state = match self.turn_state {
            TurnState::AwaitingSuccessfulResult => {
                self.push_output("\n\n")?;
                TurnState::SuccessfulResultAfterText
            }
            TurnState::AwaitingAuthoritativeText | TurnState::EmptySuccessfulResult => {
                TurnState::EmptySuccessfulResult
            }
            TurnState::SuccessfulResultAfterText => TurnState::SuccessfulResultAfterText,
        };
        if let Some(cost) = cost {
            self.cost = Some(cost);
        }
        Ok(())
    }

    fn consume_system(&mut self, json: &str) -> Result<(), ClaudeOutputError> {
        let system: SystemWire =
            serde_json::from_str(json).map_err(|_| ClaudeOutputError::Malformed)?;
        match system {
            SystemWire::Init {
                message_type,
                tools,
                mcp_servers,
                slash_commands,
                agents,
                skills,
                plugins,
                memory_paths,
                permission_mode,
                claude_code_version,
                output_style,
                capabilities,
                fast_mode_state,
                ..
            } => {
                validate_system_type(&message_type)?;
                if self.saw_init {
                    return Err(ClaudeOutputError::Malformed);
                }
                let explicit_empty_extensions = tools.as_ref().is_some_and(Vec::is_empty)
                    && mcp_servers.as_ref().is_some_and(Vec::is_empty)
                    && slash_commands.as_ref().is_some_and(Vec::is_empty)
                    && agents.as_ref().is_some_and(Vec::is_empty)
                    && skills.as_ref().is_some_and(Vec::is_empty)
                    && plugins.as_ref().is_some_and(Vec::is_empty)
                    && capabilities.as_ref().is_some_and(Vec::is_empty);
                let extensions_are_empty = tools.as_ref().is_none_or(Vec::is_empty)
                    && mcp_servers.as_ref().is_none_or(Vec::is_empty)
                    && slash_commands.as_ref().is_none_or(Vec::is_empty)
                    && agents.as_ref().is_none_or(Vec::is_empty)
                    && skills.as_ref().is_none_or(Vec::is_empty)
                    && plugins.as_ref().is_none_or(Vec::is_empty);
                if (self.contract.require_init && !explicit_empty_extensions)
                    || !extensions_are_empty
                    || memory_paths.is_some()
                {
                    return Err(ClaudeOutputError::UnisolatedRuntime);
                }
                if !self.contract.require_init
                    && capabilities.as_ref().is_some_and(|values| {
                        values.iter().any(|capability| {
                            !matches!(
                                capability.as_str(),
                                "interrupt_receipt_v1" | "msg_lifecycle_v1"
                            )
                        })
                    })
                {
                    return Err(ClaudeOutputError::UnisolatedRuntime);
                }
                match self.contract.required_version {
                    Some(expected) if claude_code_version.as_deref() == Some(expected) => {}
                    Some(_) => return Err(ClaudeOutputError::UnknownProtocolValue),
                    None => match claude_code_version.as_deref() {
                        Some("2.1.207") | None => {}
                        Some(_) => return Err(ClaudeOutputError::UnknownProtocolValue),
                    },
                }
                if self
                    .contract
                    .required_permission_mode
                    .is_some_and(|expected| permission_mode.as_deref() != Some(expected))
                {
                    return Err(ClaudeOutputError::UnisolatedRuntime);
                }
                if self.contract.require_init && output_style.as_deref() != Some("default") {
                    return Err(ClaudeOutputError::UnisolatedRuntime);
                }
                match (self.contract.require_init, fast_mode_state.as_deref()) {
                    (true, Some("off")) | (false, Some("off") | None) => {
                        self.saw_init = true;
                        Ok(())
                    }
                    (true, None) => Err(ClaudeOutputError::UnisolatedRuntime),
                    (_, Some(_)) => Err(ClaudeOutputError::UnknownProtocolValue),
                }
            }
            SystemWire::ThinkingTokens { message_type, .. } => {
                validate_system_type(&message_type)?;
                if self.contract.require_init && !self.saw_init {
                    return Err(ClaudeOutputError::UnisolatedRuntime);
                }
                Ok(())
            }
            SystemWire::HookStarted { message_type, .. }
            | SystemWire::HookResponse { message_type, .. } => {
                validate_system_type(&message_type)?;
                Err(ClaudeOutputError::UnisolatedRuntime)
            }
            SystemWire::Unknown => Err(ClaudeOutputError::UnknownProtocolValue),
        }
    }

    fn consume_stream_event(&self, json: &str) -> Result<(), ClaudeOutputError> {
        let stream: StreamEventWire =
            serde_json::from_str(json).map_err(|_| ClaudeOutputError::Malformed)?;
        if stream.message_type != "stream_event" {
            return Err(ClaudeOutputError::Malformed);
        }
        match &stream.event {
            StreamEventBodyWire::MessageStart { message } => {
                validate_assistant_message(message)?;
                for block in &message.content {
                    validate_non_authoritative_content_block(block)?;
                }
                Ok(())
            }
            StreamEventBodyWire::ContentBlockStart { content_block, .. } => {
                validate_non_authoritative_content_block(content_block)
            }
            StreamEventBodyWire::ContentBlockDelta { delta, .. } => validate_stream_delta(delta),
            StreamEventBodyWire::ContentBlockStop { .. }
            | StreamEventBodyWire::MessageStop {}
            | StreamEventBodyWire::Ping {} => Ok(()),
            StreamEventBodyWire::MessageDelta { delta, .. } => validate_message_delta(delta),
            StreamEventBodyWire::Error { .. } => Err(ClaudeOutputError::ProviderReportedError),
            StreamEventBodyWire::Unknown => Err(ClaudeOutputError::UnknownProtocolValue),
        }
    }

    fn push_output(&mut self, value: &str) -> Result<(), ClaudeOutputError> {
        let next_len = self
            .output
            .len()
            .checked_add(value.len())
            .ok_or(ClaudeOutputError::FinalOutputLimit)?;
        if next_len > MAX_FINAL_OUTPUT_BYTES {
            return Err(ClaudeOutputError::FinalOutputLimit);
        }
        self.output.push_str(value);
        Ok(())
    }

    fn finish(self) -> Result<ParsedOutput, ClaudeOutputError> {
        if self.contract.require_init && !self.saw_init {
            return Err(ClaudeOutputError::UnisolatedRuntime);
        }
        match self.turn_state {
            TurnState::SuccessfulResultAfterText => {}
            TurnState::EmptySuccessfulResult => return Err(ClaudeOutputError::EmptyOutput),
            TurnState::AwaitingAuthoritativeText | TurnState::AwaitingSuccessfulResult => {
                return Err(ClaudeOutputError::MissingResult);
            }
        }
        Ok(ParsedOutput {
            output: self.output,
            cost: self.cost,
        })
    }
}

#[derive(Deserialize)]
struct MessageEnvelope {
    #[serde(rename = "type")]
    message_type: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AssistantWire {
    Nested(Box<AssistantNestedWire>),
    Direct(AssistantDirectWire),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssistantNestedWire {
    #[serde(rename = "type")]
    message_type: String,
    message: AssistantMessageWire,
    #[serde(default, rename = "parent_tool_use_id")]
    _parent_tool_use_id: Option<String>,
    #[serde(default, rename = "session_id")]
    _session_id: Option<String>,
    #[serde(default, rename = "uuid")]
    _uuid: Option<String>,
    #[serde(default, rename = "request_id")]
    _request_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssistantDirectWire {
    #[serde(rename = "type")]
    message_type: String,
    content: Vec<AssistantContentBlockWire>,
    #[serde(default, rename = "parent_tool_use_id")]
    _parent_tool_use_id: Option<String>,
    #[serde(default, rename = "session_id")]
    _session_id: Option<String>,
    #[serde(default, rename = "uuid")]
    _uuid: Option<String>,
    #[serde(default, rename = "request_id")]
    _request_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssistantMessageWire {
    #[serde(default, rename = "model")]
    _model: Option<String>,
    #[serde(default, rename = "id")]
    _id: Option<String>,
    #[serde(default, rename = "type")]
    message_type: Option<String>,
    #[serde(default)]
    role: Option<String>,
    content: Vec<AssistantContentBlockWire>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    stop_sequence: Option<String>,
    #[serde(default, rename = "stop_details")]
    _stop_details: Option<NullOnly>,
    #[serde(default, rename = "usage")]
    _usage: Option<AssistantUsageWire>,
    #[serde(default, rename = "diagnostics")]
    _diagnostics: Option<NullOnly>,
    #[serde(default, rename = "context_management")]
    _context_management: Option<NullOnly>,
}

#[derive(Deserialize)]
struct NullOnly;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssistantUsageWire {
    #[serde(default, rename = "input_tokens")]
    _input_tokens: Option<u64>,
    #[serde(default, rename = "cache_creation_input_tokens")]
    _cache_creation_input_tokens: Option<u64>,
    #[serde(default, rename = "cache_read_input_tokens")]
    _cache_read_input_tokens: Option<u64>,
    #[serde(default, rename = "cache_creation")]
    _cache_creation: Option<CacheCreationUsageWire>,
    #[serde(default, rename = "output_tokens")]
    _output_tokens: Option<u64>,
    #[serde(default, rename = "service_tier")]
    _service_tier: Option<String>,
    #[serde(default, rename = "inference_geo")]
    _inference_geo: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheCreationUsageWire {
    #[serde(default, rename = "ephemeral_5m_input_tokens")]
    _ephemeral_5m_input_tokens: Option<u64>,
    #[serde(default, rename = "ephemeral_1h_input_tokens")]
    _ephemeral_1h_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum AssistantContentBlockWire {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking {
        #[serde(rename = "thinking")]
        _thinking: String,
        #[serde(default, rename = "signature")]
        _signature: Option<String>,
    },
    #[serde(rename = "redacted_thinking")]
    RedactedThinking {
        #[serde(rename = "data")]
        _data: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        #[serde(default, rename = "id")]
        _id: Option<String>,
        #[serde(default, rename = "name")]
        _name: Option<String>,
        #[serde(default, rename = "input")]
        _input: Option<serde_json::Value>,
        #[serde(default, rename = "caller")]
        _caller: Option<serde_json::Value>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamEventWire {
    #[serde(rename = "type")]
    message_type: String,
    event: StreamEventBodyWire,
    #[serde(default, rename = "parent_tool_use_id")]
    _parent_tool_use_id: Option<String>,
    #[serde(default, rename = "session_id")]
    _session_id: Option<String>,
    #[serde(default, rename = "uuid")]
    _uuid: Option<String>,
    #[serde(default, rename = "request_id")]
    _request_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum StreamEventBodyWire {
    #[serde(rename = "message_start")]
    MessageStart { message: AssistantMessageWire },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        #[serde(default, rename = "index")]
        _index: Option<u64>,
        content_block: AssistantContentBlockWire,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        #[serde(default, rename = "index")]
        _index: Option<u64>,
        delta: StreamDeltaWire,
    },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop {
        #[serde(default, rename = "index")]
        _index: Option<u64>,
    },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: MessageDeltaWire,
        #[serde(default, rename = "usage")]
        _usage: Option<AssistantUsageWire>,
    },
    #[serde(rename = "message_stop")]
    MessageStop {},
    #[serde(rename = "ping")]
    Ping {},
    #[serde(rename = "error")]
    Error {
        #[serde(rename = "error")]
        _error: StreamErrorWire,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamErrorWire {
    #[serde(rename = "type")]
    _error_type: String,
    #[serde(rename = "message")]
    _message: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum StreamDeltaWire {
    #[serde(rename = "text_delta")]
    Text {
        #[serde(rename = "text")]
        _text: String,
    },
    #[serde(rename = "thinking_delta")]
    Thinking {
        #[serde(rename = "thinking")]
        _thinking: String,
    },
    #[serde(rename = "signature_delta")]
    Signature {
        #[serde(rename = "signature")]
        _signature: String,
    },
    #[serde(rename = "input_json_delta")]
    InputJson {
        #[serde(rename = "partial_json")]
        _partial_json: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageDeltaWire {
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    stop_sequence: Option<String>,
}

fn validate_assistant_message(message: &AssistantMessageWire) -> Result<(), ClaudeOutputError> {
    match message.message_type.as_deref() {
        Some("message") | None => {}
        Some(_) => return Err(ClaudeOutputError::UnknownProtocolValue),
    }
    match message.role.as_deref() {
        Some("assistant") | None => {}
        Some(_) => return Err(ClaudeOutputError::UnknownProtocolValue),
    }
    validate_stop_fields(
        message.stop_reason.as_deref(),
        message.stop_sequence.as_deref(),
    )
}

fn validate_non_authoritative_content_block(
    block: &AssistantContentBlockWire,
) -> Result<(), ClaudeOutputError> {
    match block {
        AssistantContentBlockWire::Text { .. }
        | AssistantContentBlockWire::Thinking { .. }
        | AssistantContentBlockWire::RedactedThinking { .. } => Ok(()),
        AssistantContentBlockWire::ToolUse { .. } => Err(ClaudeOutputError::ToolProtocol),
        AssistantContentBlockWire::Unknown => Err(ClaudeOutputError::UnknownProtocolValue),
    }
}

fn validate_stream_delta(delta: &StreamDeltaWire) -> Result<(), ClaudeOutputError> {
    match delta {
        StreamDeltaWire::Text { .. }
        | StreamDeltaWire::Thinking { .. }
        | StreamDeltaWire::Signature { .. } => Ok(()),
        StreamDeltaWire::InputJson { .. } => Err(ClaudeOutputError::ToolProtocol),
        StreamDeltaWire::Unknown => Err(ClaudeOutputError::UnknownProtocolValue),
    }
}

fn validate_message_delta(delta: &MessageDeltaWire) -> Result<(), ClaudeOutputError> {
    validate_stop_fields(delta.stop_reason.as_deref(), delta.stop_sequence.as_deref())
}

fn validate_stop_fields(
    stop_reason: Option<&str>,
    stop_sequence: Option<&str>,
) -> Result<(), ClaudeOutputError> {
    validate_stop_reason(stop_reason)?;
    if stop_sequence.is_some() && stop_reason != Some("stop_sequence") {
        return Err(ClaudeOutputError::Malformed);
    }
    Ok(())
}

fn validate_stop_reason(stop_reason: Option<&str>) -> Result<(), ClaudeOutputError> {
    match stop_reason {
        Some("tool_use") => Err(ClaudeOutputError::ToolProtocol),
        Some("end_turn" | "max_tokens" | "stop_sequence" | "pause_turn" | "refusal") | None => {
            Ok(())
        }
        Some(_) => Err(ClaudeOutputError::UnknownProtocolValue),
    }
}

#[derive(Deserialize)]
#[serde(tag = "subtype", deny_unknown_fields)]
enum SystemWire {
    #[serde(rename = "init")]
    Init {
        #[serde(rename = "type")]
        message_type: String,
        #[serde(default, rename = "cwd")]
        _cwd: Option<String>,
        #[serde(default, rename = "session_id")]
        _session_id: Option<String>,
        tools: Option<Vec<String>>,
        mcp_servers: Option<Vec<McpServerWire>>,
        #[serde(default, rename = "model")]
        _model: Option<String>,
        #[serde(default, rename = "permissionMode")]
        permission_mode: Option<String>,
        slash_commands: Option<Vec<String>>,
        #[serde(default, rename = "apiKeySource")]
        _api_key_source: Option<String>,
        #[serde(default)]
        claude_code_version: Option<String>,
        #[serde(default, rename = "output_style")]
        output_style: Option<String>,
        agents: Option<Vec<String>>,
        skills: Option<Vec<String>>,
        plugins: Option<Vec<PluginWire>>,
        capabilities: Option<Vec<String>>,
        #[serde(default, rename = "analytics_disabled")]
        _analytics_disabled: Option<bool>,
        #[serde(default, rename = "product_feedback_disabled")]
        _product_feedback_disabled: Option<bool>,
        #[serde(default, rename = "uuid")]
        _uuid: Option<String>,
        #[serde(default)]
        memory_paths: Option<MemoryPathsWire>,
        #[serde(default)]
        fast_mode_state: Option<String>,
    },
    #[serde(rename = "thinking_tokens")]
    ThinkingTokens {
        #[serde(rename = "type")]
        message_type: String,
        #[serde(rename = "estimated_tokens")]
        _estimated_tokens: u64,
        #[serde(rename = "estimated_tokens_delta")]
        _estimated_tokens_delta: u64,
        #[serde(default, rename = "uuid")]
        _uuid: Option<String>,
        #[serde(default, rename = "session_id")]
        _session_id: Option<String>,
    },
    #[serde(rename = "hook_started")]
    HookStarted {
        #[serde(rename = "type")]
        message_type: String,
        #[serde(rename = "hook_id")]
        _hook_id: String,
        #[serde(rename = "hook_name")]
        _hook_name: String,
        #[serde(rename = "hook_event")]
        _hook_event: String,
        #[serde(default, rename = "uuid")]
        _uuid: Option<String>,
        #[serde(default, rename = "session_id")]
        _session_id: Option<String>,
    },
    #[serde(rename = "hook_response")]
    HookResponse {
        #[serde(rename = "type")]
        message_type: String,
        #[serde(rename = "hook_id")]
        _hook_id: String,
        #[serde(rename = "hook_name")]
        _hook_name: String,
        #[serde(rename = "hook_event")]
        _hook_event: String,
        #[serde(rename = "output")]
        _output: String,
        #[serde(rename = "stdout")]
        _stdout: String,
        #[serde(rename = "stderr")]
        _stderr: String,
        #[serde(rename = "exit_code")]
        _exit_code: i64,
        #[serde(rename = "outcome")]
        _outcome: String,
        #[serde(default, rename = "uuid")]
        _uuid: Option<String>,
        #[serde(default, rename = "session_id")]
        _session_id: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct McpServerWire {
    #[serde(rename = "name")]
    _name: String,
    #[serde(rename = "status")]
    _status: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginWire {
    #[serde(rename = "name")]
    _name: String,
    #[serde(rename = "path")]
    _path: String,
    #[serde(rename = "source")]
    _source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryPathsWire {
    #[serde(rename = "auto")]
    _auto: String,
}

fn validate_system_type(message_type: &str) -> Result<(), ClaudeOutputError> {
    if message_type == "system" {
        Ok(())
    } else {
        Err(ClaudeOutputError::Malformed)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultWire {
    #[serde(rename = "type")]
    message_type: String,
    subtype: String,
    #[serde(default)]
    is_error: Option<bool>,
    #[serde(default, rename = "api_error_status")]
    _api_error_status: Option<NullOnly>,
    #[serde(default, rename = "duration_ms")]
    _duration_ms: Option<u64>,
    #[serde(default, rename = "duration_api_ms")]
    _duration_api_ms: Option<u64>,
    #[serde(default, rename = "ttft_ms")]
    _ttft_ms: Option<u64>,
    #[serde(default, rename = "ttft_stream_ms")]
    _ttft_stream_ms: Option<u64>,
    #[serde(default, rename = "time_to_request_ms")]
    _time_to_request_ms: Option<u64>,
    #[serde(default, rename = "num_turns")]
    _num_turns: Option<u64>,
    #[serde(default, rename = "result")]
    _result: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default, rename = "session_id")]
    _session_id: Option<String>,
    #[serde(default)]
    cost: Option<CostWire>,
    #[serde(default)]
    total_cost_usd: Option<f64>,
    #[serde(default)]
    usage: Option<UsageWire>,
    #[serde(default, rename = "modelUsage")]
    model_usage: Option<BTreeMap<String, ModelUsageWire>>,
    #[serde(default)]
    permission_denials: Option<Vec<PermissionDenialWire>>,
    #[serde(default)]
    terminal_reason: Option<String>,
    #[serde(default)]
    fast_mode_state: Option<String>,
    #[serde(default, rename = "uuid")]
    _uuid: Option<String>,
    #[serde(default)]
    error_message: Option<String>,
}

impl ResultWire {
    fn into_success_cost(self) -> Result<Option<CostInfo>, ClaudeOutputError> {
        if self.message_type != "result" {
            return Err(ClaudeOutputError::Malformed);
        }
        match self.subtype.as_str() {
            "error" => return Err(ClaudeOutputError::ProviderReportedError),
            "success" => {}
            _ => return Err(ClaudeOutputError::UnknownProtocolValue),
        }
        if self.is_error == Some(true) || self.error_message.is_some() {
            return Err(ClaudeOutputError::ProviderReportedError);
        }
        validate_result_stop_reason(self.stop_reason.as_deref())?;
        match self.terminal_reason.as_deref() {
            Some("completed") | None => {}
            Some(_) => return Err(ClaudeOutputError::UnknownProtocolValue),
        }
        match self.fast_mode_state.as_deref() {
            Some("off") | None => {}
            Some(_) => return Err(ClaudeOutputError::UnknownProtocolValue),
        }
        if self
            .permission_denials
            .as_ref()
            .is_some_and(|denials| !denials.is_empty())
        {
            return Err(ClaudeOutputError::ToolProtocol);
        }
        self.into_cost()
    }

    fn into_cost(self) -> Result<Option<CostInfo>, ClaudeOutputError> {
        let has_modern_telemetry =
            self.total_cost_usd.is_some() || self.usage.is_some() || self.model_usage.is_some();
        if self.cost.is_some() && has_modern_telemetry {
            return Err(ClaudeOutputError::InvalidCost);
        }
        if let Some(cost) = self.cost {
            return cost.into_cost().map(Some);
        }
        let Some(total_cost_usd) = self.total_cost_usd else {
            if has_modern_telemetry {
                return Err(ClaudeOutputError::InvalidCost);
            }
            return Ok(None);
        };
        let Some(usage) = self.usage else {
            if self.model_usage.is_some() {
                return Err(ClaudeOutputError::InvalidCost);
            }
            if total_cost_usd == 0.0 {
                return Ok(None);
            }
            return CostInfo::new(0, 0, total_cost_usd, 0, 0)
                .map(Some)
                .map_err(|_| ClaudeOutputError::InvalidCost);
        };
        usage.validate()?;
        if let Some(model_usage) = self.model_usage.as_ref() {
            validate_model_usage(model_usage, &usage, total_cost_usd)?;
        }
        let input_tokens = usage
            .input_tokens
            .checked_add(usage.cache_creation_input_tokens)
            .and_then(|tokens| tokens.checked_add(usage.cache_read_input_tokens))
            .ok_or(ClaudeOutputError::InvalidCost)?;
        CostInfo::new(
            input_tokens,
            usage.output_tokens,
            total_cost_usd,
            usage.cache_creation_input_tokens,
            usage.cache_read_input_tokens,
        )
        .map(Some)
        .map_err(|_| ClaudeOutputError::InvalidCost)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageWire {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    server_tool_use: Option<ServerToolUseWire>,
    #[serde(default)]
    service_tier: Option<String>,
    #[serde(default)]
    cache_creation: Option<ResultCacheCreationUsageWire>,
    #[serde(default)]
    inference_geo: Option<String>,
    #[serde(default)]
    iterations: Option<Vec<IterationUsageWire>>,
    #[serde(default)]
    speed: Option<String>,
}

impl UsageWire {
    fn validate(&self) -> Result<(), ClaudeOutputError> {
        if let Some(server_tool_use) = &self.server_tool_use {
            server_tool_use.validate()?;
        }
        if let Some(cache_creation) = &self.cache_creation {
            cache_creation.validate_total(self.cache_creation_input_tokens)?;
        }
        match self.service_tier.as_deref() {
            Some("standard" | "priority" | "batch") | None => {}
            Some(_) => return Err(ClaudeOutputError::UnknownProtocolValue),
        }
        match self.inference_geo.as_deref() {
            Some("not_available") | None => {}
            Some(_) => return Err(ClaudeOutputError::UnknownProtocolValue),
        }
        match self.speed.as_deref() {
            Some("standard") | None => {}
            Some(_) => return Err(ClaudeOutputError::UnknownProtocolValue),
        }
        for iteration in self.iterations.as_deref().unwrap_or_default() {
            iteration.validate()?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerToolUseWire {
    #[serde(default)]
    web_search_requests: u64,
    #[serde(default)]
    web_fetch_requests: u64,
}

impl ServerToolUseWire {
    fn validate(&self) -> Result<(), ClaudeOutputError> {
        if self.web_search_requests == 0 && self.web_fetch_requests == 0 {
            Ok(())
        } else {
            Err(ClaudeOutputError::ToolProtocol)
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultCacheCreationUsageWire {
    #[serde(default)]
    ephemeral_5m_input_tokens: u64,
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
}

impl ResultCacheCreationUsageWire {
    fn validate_total(&self, expected: u64) -> Result<(), ClaudeOutputError> {
        let observed = self
            .ephemeral_5m_input_tokens
            .checked_add(self.ephemeral_1h_input_tokens)
            .ok_or(ClaudeOutputError::InvalidCost)?;
        if observed == expected {
            Ok(())
        } else {
            Err(ClaudeOutputError::InvalidCost)
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IterationUsageWire {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_creation: Option<ResultCacheCreationUsageWire>,
    #[serde(rename = "type")]
    iteration_type: String,
}

impl IterationUsageWire {
    fn validate(&self) -> Result<(), ClaudeOutputError> {
        if self.iteration_type != "message" {
            return Err(ClaudeOutputError::UnknownProtocolValue);
        }
        if let Some(cache_creation) = &self.cache_creation {
            cache_creation.validate_total(self.cache_creation_input_tokens)?;
        }
        let _ = self
            .input_tokens
            .checked_add(self.output_tokens)
            .and_then(|tokens| tokens.checked_add(self.cache_read_input_tokens))
            .and_then(|tokens| tokens.checked_add(self.cache_creation_input_tokens))
            .ok_or(ClaudeOutputError::InvalidCost)?;
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ModelUsageWire {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
    web_search_requests: u64,
    #[serde(rename = "costUSD")]
    cost_usd: f64,
    context_window: u64,
    max_output_tokens: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionDenialWire {
    #[serde(rename = "tool_name")]
    _tool_name: String,
    #[serde(default, rename = "tool_use_id")]
    _tool_use_id: Option<String>,
    #[serde(default, rename = "tool_input")]
    _tool_input: Option<DeniedToolInputWire>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeniedToolInputWire {
    #[serde(default, rename = "command")]
    _command: Option<String>,
    #[serde(default, rename = "description")]
    _description: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CostWire {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    total_cost_usd: f64,
    #[serde(default)]
    cache_creation_tokens: u64,
    #[serde(default)]
    cache_read_tokens: u64,
}

fn validate_model_usage(
    model_usage: &BTreeMap<String, ModelUsageWire>,
    usage: &UsageWire,
    total_cost_usd: f64,
) -> Result<(), ClaudeOutputError> {
    if model_usage.is_empty() {
        return Err(ClaudeOutputError::InvalidCost);
    }
    let mut input_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    let mut cache_read_input_tokens = 0_u64;
    let mut cache_creation_input_tokens = 0_u64;
    let mut cost_usd = 0.0;
    for (model, model_cost) in model_usage {
        if model_cost.web_search_requests != 0 {
            return Err(ClaudeOutputError::ToolProtocol);
        }
        if model.is_empty()
            || model_cost.context_window == 0
            || model_cost.max_output_tokens == 0
            || !model_cost.cost_usd.is_finite()
            || model_cost.cost_usd < 0.0
        {
            return Err(ClaudeOutputError::InvalidCost);
        }
        input_tokens = input_tokens
            .checked_add(model_cost.input_tokens)
            .ok_or(ClaudeOutputError::InvalidCost)?;
        output_tokens = output_tokens
            .checked_add(model_cost.output_tokens)
            .ok_or(ClaudeOutputError::InvalidCost)?;
        cache_read_input_tokens = cache_read_input_tokens
            .checked_add(model_cost.cache_read_input_tokens)
            .ok_or(ClaudeOutputError::InvalidCost)?;
        cache_creation_input_tokens = cache_creation_input_tokens
            .checked_add(model_cost.cache_creation_input_tokens)
            .ok_or(ClaudeOutputError::InvalidCost)?;
        cost_usd += model_cost.cost_usd;
    }
    let token_totals_match = input_tokens == usage.input_tokens
        && output_tokens == usage.output_tokens
        && cache_read_input_tokens == usage.cache_read_input_tokens
        && cache_creation_input_tokens == usage.cache_creation_input_tokens;
    let cost_tolerance = f64::EPSILON * 16.0 * total_cost_usd.abs().max(1.0);
    if !token_totals_match
        || !cost_usd.is_finite()
        || (cost_usd - total_cost_usd).abs() > cost_tolerance
    {
        return Err(ClaudeOutputError::InvalidCost);
    }
    Ok(())
}

fn validate_result_stop_reason(stop_reason: Option<&str>) -> Result<(), ClaudeOutputError> {
    match stop_reason {
        Some("tool_use") => Err(ClaudeOutputError::ToolProtocol),
        Some("end_turn") | None => Ok(()),
        Some("max_tokens" | "stop_sequence" | "pause_turn" | "refusal") => {
            Err(ClaudeOutputError::ProviderReportedError)
        }
        Some(_) => Err(ClaudeOutputError::UnknownProtocolValue),
    }
}

impl CostWire {
    fn into_cost(self) -> Result<CostInfo, ClaudeOutputError> {
        CostInfo::new(
            self.input_tokens,
            self.output_tokens,
            self.total_cost_usd,
            self.cache_creation_tokens,
            self.cache_read_tokens,
        )
        .map_err(|_| ClaudeOutputError::InvalidCost)
    }
}

/// Recursively rejects JSON whose meaning depends on a duplicate-key policy.
#[derive(Clone, Copy)]
struct UniqueJson;

impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueJsonVisitor)
    }
}

struct UniqueJsonVisitor;

impl<'de> Visitor<'de> for UniqueJsonVisitor {
    type Value = UniqueJson;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        UniqueJson::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<UniqueJson>()?.is_some() {}
        Ok(UniqueJson)
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate JSON object key"));
            }
            map.next_value::<UniqueJson>()?;
        }
        Ok(UniqueJson)
    }
}

fn reject_duplicate_object_keys(json: &str) -> Result<(), ClaudeOutputError> {
    let mut deserializer = serde_json::Deserializer::from_str(json);
    UniqueJson::deserialize(&mut deserializer).map_err(|_| ClaudeOutputError::Malformed)?;
    deserializer.end().map_err(|_| ClaudeOutputError::Malformed)
}

#[cfg(any(test, feature = "experimental-first-use-pilot"))]
mod first_use_pilot_wire;

#[cfg(test)]
mod tests;
