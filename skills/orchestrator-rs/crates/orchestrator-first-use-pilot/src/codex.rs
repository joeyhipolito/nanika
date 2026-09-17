//! Native, non-enrolled Codex 0.154.0 first-use executors.
use orchestrator_exec::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};
pub const VERSION: &str = "0.154.0";
const COMMON_PREFIX: &[&str] = &[
    "exec",
    "--ignore-user-config",
    "--ignore-rules",
    "--ephemeral",
    "--skip-git-repo-check",
];
const COMMON_POLICY: &[&str] = &[
    "--json",
    "--color",
    "never",
    "-c",
    "approval_policy=\"never\"",
    "-c",
    "web_search=\"disabled\"",
    "-c",
    "project_doc_max_bytes=0",
];
const COMMON_DISABLED_FEATURES: &[&str] = &[
    "--disable",
    "shell_snapshot",
    "--disable",
    "hooks",
    "--disable",
    "plugins",
    "--disable",
    "apps",
    "--disable",
    "multi_agent",
    "--disable",
    "multi_agent_v2",
    "--disable",
    "browser_use",
    "--disable",
    "browser_use_external",
    "--disable",
    "in_app_browser",
    "--disable",
    "computer_use",
    "--disable",
    "image_generation",
    "--disable",
    "view_image",
    "--disable",
    "skill_search",
    "--disable",
    "skill_mcp_dependency_install",
    "--disable",
    "memories",
    "--disable",
    "goals",
    "--disable",
    "sleep_tool",
];
const COMMON_TAIL: &[&str] = &[
    "--enable",
    "skip_host_skill_discovery",
    "-c",
    "suppress_unstable_features_warning=true",
];
const CONFIG_WARNING_PREFIX: &str = "Under-development features enabled: skip_host_skill_discovery. Under-development features are incomplete and may behave unpredictably. To suppress this warning, set `suppress_unstable_features_warning = true` in ";
const CODE_MODE_WARNING: &str = "Code Mode is unavailable because code-mode host is disabled. Code mode will fail closed; enable `features.code_mode_host` and install `codex-code-mode-host`.";

fn known_startup_warning(message: &str) -> bool {
    if message == CODE_MODE_WARNING {
        return true;
    }
    // The provider's config location varies per user; only that path may vary.
    message
        .strip_prefix(CONFIG_WARNING_PREFIX)
        .and_then(|path| path.strip_suffix('.'))
        .is_some_and(|path| {
            path.len() <= 4096
                && !path.chars().any(char::is_control)
                && Path::new(path).is_absolute()
                && Path::new(path).ends_with(".codex/config.toml")
        })
}

#[test]
fn startup_warning_accepts_only_the_known_notice_with_an_absolute_config_path() {
    for path in [
        "/home/fixture/.codex/config.toml",
        "/home/a user/.codex/config.toml",
    ] {
        assert!(known_startup_warning(&format!(
            "{CONFIG_WARNING_PREFIX}{path}."
        )));
    }
    for path in [
        "relative/.codex/config.toml",
        "/home/x/other.toml",
        "/home/x/.codex/config.toml\nerror",
        "/home/x/.codex/config.toml. execution failed",
    ] {
        assert!(!known_startup_warning(&format!(
            "{CONFIG_WARNING_PREFIX}{path}."
        )));
    }
    assert!(!known_startup_warning("provider error"));
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Review,
    Code,
}

pub struct CodexExecutor {
    runtime: RuntimeFamily,
    mode: Mode,
    executable_id: String,
    environment: Vec<(String, String)>,
    portal_directory: Option<std::path::PathBuf>,
}
impl CodexExecutor {
    pub(crate) fn new_with_process_binding(
        mode: Mode,
        executable_id: impl Into<String>,
        environment: Vec<(String, String)>,
    ) -> Result<Self, ContractError> {
        Ok(Self {
            runtime: RuntimeFamily::parse("codex")?,
            mode,
            executable_id: executable_id.into(),
            environment,
            portal_directory: None,
        })
    }

    pub(crate) fn with_portal_directory(mut self, directory: Option<std::path::PathBuf>) -> Self {
        self.portal_directory = directory;
        self
    }

    pub(crate) fn process_request(
        &self,
        request: &ExecutionRequest,
    ) -> Result<ProcessRequest, ServiceContractError> {
        let mut process = ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            &self.executable_id,
            request.worker_dir(),
        )?;
        for flag in COMMON_PREFIX {
            process = process.with_argument(*flag)?;
        }
        process = process
            .with_argument("--sandbox")?
            .with_argument(match self.mode {
                Mode::Review => "read-only",
                Mode::Code => "workspace-write",
            })?;
        if let (Mode::Code, Some(directory)) = (self.mode, &self.portal_directory) {
            process = process
                .with_argument("--add-dir")?
                .with_argument(directory.to_string_lossy())?;
        }
        for flag in COMMON_POLICY {
            process = process.with_argument(*flag)?;
        }
        let feature_switch = match self.mode {
            Mode::Review => "--disable",
            Mode::Code => "--enable",
        };
        process = process
            .with_argument(feature_switch)?
            .with_argument("shell_tool")?
            .with_argument(feature_switch)?
            .with_argument("unified_exec")?;
        for flag in COMMON_DISABLED_FEATURES {
            process = process.with_argument(*flag)?;
        }
        process = process
            .with_argument(feature_switch)?
            .with_argument("code_mode_host")?;
        for flag in COMMON_TAIL {
            process = process.with_argument(*flag)?;
        }
        for (name, value) in &self.environment {
            process = process.with_environment(name, value)?;
        }
        if self.mode == Mode::Code {
            process = process
                .with_argument("-c")?
                .with_argument("sandbox_workspace_write.network_access=false")?;
        }
        process
            .with_argument("-c")?
            .with_argument(format!(
                "model_reasoning_effort=\"{}\"",
                request.effort().as_str()
            ))?
            .with_argument("-m")?
            .with_argument(request.model())?
            .with_argument("--")?
            .with_argument("-")?
            .with_stdin(request.objective().as_bytes().to_vec())?
            .with_max_output_bytes(32 * 1024 * 1024)
    }
}
fn failed(elapsed: Duration, mode: Mode) -> AttemptOutcome {
    let detail = match mode {
        Mode::Review => "Codex wire or request violated the tool-less pilot contract",
        Mode::Code => "Codex wire, request, or coding tool violated the coding pilot contract",
    };
    AttemptOutcome::incomplete(
        MechanicalTermination::ContractViolation,
        Failure::new(FailureKind::Protocol, detail).ok(),
        PartialWork::empty(),
        elapsed,
    )
}
impl PhaseExecutor for CodexExecutor {
    fn descriptor(&self) -> Option<RuntimeDescriptor> {
        Some(RuntimeDescriptor::new(
            self.runtime.clone(),
            RuntimeCaps {
                tool_use: self.mode == Mode::Code,
                session_resume: false,
                streaming: false,
                cost_report: false,
                artifacts: self.mode == Mode::Code,
            },
        ))
    }
    fn execute(
        &self,
        dispatch: DispatchRequest<'_>,
        context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome {
        let request = dispatch.request();
        let target_matches = match (self.mode, request.target_dir()) {
            (Mode::Review, None) => true,
            (Mode::Code, Some(target)) => target == request.worker_dir(),
            _ => false,
        };
        if !target_matches
            || request.resume_from().is_some()
            || request.max_turns() != 1
            || (self.portal_directory.is_some() && self.mode != Mode::Code)
        {
            return failed(Duration::ZERO, self.mode);
        }
        let process = match self.process_request(request) {
            Ok(p) => p,
            Err(_) => return failed(Duration::ZERO, self.mode),
        };
        let receipt = match context.run_process(&process) {
            Ok(r) => r,
            Err(_) => {
                return AttemptOutcome::incomplete(
                    MechanicalTermination::SupervisorFailure,
                    None,
                    PartialWork::empty(),
                    Duration::ZERO,
                );
            }
        };
        if !receipt.is_success() {
            let termination = match receipt.termination() {
                ProcessTerminationReceipt::Exited(s) => MechanicalTermination::ProcessExited(s),
                ProcessTerminationReceipt::Cancelled => MechanicalTermination::Cancelled,
                ProcessTerminationReceipt::DeadlineExceeded => {
                    MechanicalTermination::HardDeadlineExceeded
                }
                ProcessTerminationReceipt::Stalled => MechanicalTermination::WatchdogStalled,
                _ => MechanicalTermination::SupervisorFailure,
            };
            return AttemptOutcome::incomplete(
                termination,
                None,
                PartialWork::empty(),
                receipt.elapsed(),
            );
        }
        let parsed = match self.mode {
            Mode::Review => parse(receipt.expose_stdout()).map(|parsed| (parsed.output, false)),
            Mode::Code => parse_code(receipt.expose_stdout(), request.worker_dir())
                .map(|parsed| (parsed.output, parsed.tool_failed)),
        };
        match parsed {
            Err(_) => failed(receipt.elapsed(), self.mode),
            Ok((output, _)) => {
                AttemptOutcome::completed(output, AttemptEvidence::new(), receipt.elapsed())
                    .unwrap_or_else(|_| failed(receipt.elapsed(), self.mode))
            }
        }
    }
}

// Closed typed structs reject duplicate fields at every nesting level. No Value
// normalization occurs before decoding; unknown event/item variants fail closed.
#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Event {
    #[serde(rename = "thread.started")]
    Thread { thread_id: String },
    #[serde(rename = "turn.started")]
    Start {},
    #[serde(rename = "item.completed")]
    Item { item: Item },
    #[serde(rename = "turn.completed")]
    Complete { usage: Usage },
}
#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Item {
    #[serde(rename = "error")]
    Error { id: String, message: String },
    #[serde(rename = "agent_message")]
    Answer { id: String, text: String },
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
}
pub struct Parsed {
    pub output: String,
    pub warnings: Vec<String>,
    pub usage: Usage,
}

pub(crate) fn completed_usage_value(bytes: &[u8]) -> Option<serde_json::Value> {
    let mut usage = None;
    for line in std::str::from_utf8(bytes).ok()?.lines() {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        if value.get("type").and_then(serde_json::Value::as_str) != Some("turn.completed") {
            continue;
        }
        if usage.is_some() {
            return None;
        }
        let parsed: Usage = serde_json::from_value(value.get("usage")?.clone()).ok()?;
        if parsed.cached_input_tokens > parsed.input_tokens
            || parsed.reasoning_output_tokens > parsed.output_tokens
        {
            return None;
        }
        usage = serde_json::to_value(parsed).ok();
    }
    usage
}
pub fn parse(bytes: &[u8]) -> Result<Parsed, &'static str> {
    let text = std::str::from_utf8(bytes).map_err(|_| "UTF-8")?;
    let mut state = 0;
    let mut ids = BTreeSet::new();
    let mut warnings = Vec::new();
    let mut output = None;
    let mut usage = None;
    for line in text.lines() {
        let event: Event = serde_json::from_str(line).map_err(|_| "malformed or unknown record")?;
        match event {
            Event::Thread { thread_id } if state == 0 && !thread_id.trim().is_empty() => state = 1,
            Event::Start {} if state == 1 => state = 2,
            Event::Item {
                item: Item::Error { id, message },
            } if state == 1 => {
                if id.is_empty()
                    || !ids.insert(id)
                    || !known_startup_warning(&message)
                    || warnings.contains(&message)
                {
                    return Err("unknown or duplicate startup notice");
                }
                warnings.push(message);
            }
            Event::Item {
                item: Item::Answer { id, text },
            } if state == 2 && output.is_none() => {
                if id.is_empty() || !ids.insert(id) || text.trim().is_empty() {
                    return Err("empty or duplicate answer");
                }
                output = Some(text);
            }
            Event::Complete { usage: u } if state == 2 && output.is_some() => {
                if u.cached_input_tokens > u.input_tokens
                    || u.reasoning_output_tokens > u.output_tokens
                {
                    return Err("invalid usage");
                }
                usage = Some(u);
                state = 3;
            }
            _ => return Err("ambiguous ordering or forbidden activity"),
        }
    }
    if state != 3 {
        return Err("missing terminal event");
    }
    Ok(Parsed {
        output: output.ok_or("missing answer")?,
        warnings,
        usage: usage.ok_or("missing usage")?,
    })
}

#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum CodeEvent {
    #[serde(rename = "thread.started")]
    Thread { thread_id: String },
    #[serde(rename = "turn.started")]
    Start {},
    #[serde(rename = "item.started")]
    ItemStarted { item: CodeItem },
    #[serde(rename = "item.completed")]
    ItemCompleted { item: CodeItem },
    #[serde(rename = "turn.completed")]
    Complete { usage: Usage },
}

#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum CodeItem {
    #[serde(rename = "agent_message")]
    Answer { id: String, text: String },
    #[serde(rename = "command_execution")]
    Command {
        id: String,
        command: String,
        aggregated_output: String,
        exit_code: Option<i32>,
        status: ToolStatus,
    },
    #[serde(rename = "file_change")]
    FileChange {
        id: String,
        changes: Vec<FileChange>,
        status: ToolStatus,
    },
    #[serde(rename = "collab_tool_call")]
    EmptyWait {
        id: String,
        tool: EmptyWaitTool,
        sender_thread_id: String,
        receiver_thread_ids: Vec<String>,
        prompt: (),
        agents_states: EmptyAgentStates,
        status: ToolStatus,
    },
}

#[derive(Deserialize)]
enum EmptyWaitTool {
    #[serde(rename = "wait")]
    Wait,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyAgentStates {}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ToolStatus {
    InProgress,
    Completed,
    Failed,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct FileChange {
    path: String,
    kind: FileChangeKind,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum FileChangeKind {
    Update,
    Add,
    Delete,
}

#[derive(Eq, PartialEq)]
enum PendingTool {
    Command(String),
    FileChange(Vec<FileChange>),
    EmptyWait,
}

pub(crate) struct CommandEvidence {
    pub(crate) id: String,
    pub(crate) command: String,
    pub(crate) output: String,
    pub(crate) exit_code: i32,
}

pub struct CodeParsed {
    pub output: String,
    pub usage: Usage,
    pub command_count: u64,
    pub file_change_count: u64,
    pub tool_failed: bool,
    pub(crate) commands: Vec<CommandEvidence>,
}

pub fn parse_code(bytes: &[u8], workspace: &Path) -> Result<CodeParsed, &'static str> {
    parse_code_inner(bytes, workspace, false)
}

pub(crate) fn parse_code_commands(
    bytes: &[u8],
    workspace: &Path,
) -> Result<CodeParsed, &'static str> {
    parse_code_inner(bytes, workspace, true)
}

fn parse_code_inner(
    bytes: &[u8],
    workspace: &Path,
    capture_commands: bool,
) -> Result<CodeParsed, &'static str> {
    let text = std::str::from_utf8(bytes).map_err(|_| "UTF-8")?;
    let workspace = workspace
        .canonicalize()
        .map_err(|_| "workspace is not canonicalizable")?;
    let mut state = 0;
    let mut active_thread = None;
    let mut ids = BTreeSet::new();
    let mut pending = BTreeMap::new();
    let mut output = None;
    let mut last_item_was_answer = false;
    let mut usage = None;
    let mut command_count = 0_u64;
    let mut commands = Vec::new();
    let mut file_change_count = 0_u64;
    let mut tool_failed = false;
    for line in text.lines() {
        let event: CodeEvent =
            serde_json::from_str(line).map_err(|_| "malformed or unknown coding record")?;
        if pending
            .first_key_value()
            .is_some_and(|(_, item)| matches!(item, PendingTool::EmptyWait))
            && !matches!(
                &event,
                CodeEvent::ItemCompleted {
                    item: CodeItem::EmptyWait { .. }
                }
            )
        {
            return Err("empty wait start was not immediately paired");
        }
        match event {
            CodeEvent::Thread { thread_id } if state == 0 && !thread_id.trim().is_empty() => {
                active_thread = Some(thread_id);
                state = 1;
            }
            CodeEvent::Start {} if state == 1 => state = 2,
            CodeEvent::ItemCompleted {
                item: CodeItem::Answer { id, text },
            } if state == 2 => {
                if id.is_empty() || !ids.insert(id) || text.trim().is_empty() {
                    return Err("empty or duplicate coding answer");
                }
                output = Some(text);
                last_item_was_answer = true;
            }
            CodeEvent::ItemStarted {
                item:
                    CodeItem::Command {
                        id,
                        command,
                        aggregated_output,
                        exit_code,
                        status,
                    },
            } if state == 2 => {
                last_item_was_answer = false;
                if id.is_empty()
                    || !ids.insert(id.clone())
                    || command.trim().is_empty()
                    || !aggregated_output.is_empty()
                    || exit_code.is_some()
                    || status != ToolStatus::InProgress
                    || pending.insert(id, PendingTool::Command(command)).is_some()
                {
                    return Err("invalid command start");
                }
            }
            CodeEvent::ItemCompleted {
                item:
                    CodeItem::Command {
                        id,
                        command,
                        aggregated_output,
                        exit_code,
                        status,
                    },
            } if state == 2 => {
                last_item_was_answer = false;
                match pending.remove(&id) {
                    Some(PendingTool::Command(start)) if start == command => {}
                    _ => return Err("command completion did not match its start"),
                }
                let exit_code = exit_code.ok_or("command completion omitted exit code")?;
                let valid_status = match status {
                    ToolStatus::Completed => true,
                    ToolStatus::Failed => exit_code != 0,
                    ToolStatus::InProgress => false,
                };
                if !valid_status {
                    return Err("inconsistent command completion status");
                }
                tool_failed |= exit_code != 0;
                command_count = command_count.checked_add(1).ok_or("too many commands")?;
                if capture_commands {
                    if commands.len() >= 256 {
                        return Err("too many Portal commands");
                    }
                    commands.push(CommandEvidence {
                        id,
                        command,
                        output: aggregated_output,
                        exit_code,
                    });
                }
            }
            CodeEvent::ItemStarted {
                item:
                    CodeItem::FileChange {
                        id,
                        changes,
                        status,
                    },
            } if state == 2 => {
                last_item_was_answer = false;
                validate_changes(&changes, &workspace)?;
                if id.is_empty()
                    || !ids.insert(id.clone())
                    || changes.is_empty()
                    || status != ToolStatus::InProgress
                    || pending
                        .insert(id, PendingTool::FileChange(changes))
                        .is_some()
                {
                    return Err("invalid file-change start");
                }
            }
            CodeEvent::ItemCompleted {
                item:
                    CodeItem::FileChange {
                        id,
                        changes,
                        status,
                    },
            } if state == 2 => {
                last_item_was_answer = false;
                validate_changes(&changes, &workspace)?;
                if status != ToolStatus::Completed
                    || pending.remove(&id) != Some(PendingTool::FileChange(changes))
                {
                    return Err("file-change completion did not match its start");
                }
                file_change_count = file_change_count
                    .checked_add(1)
                    .ok_or("too many file changes")?;
            }
            CodeEvent::ItemStarted {
                item:
                    CodeItem::EmptyWait {
                        id,
                        tool: EmptyWaitTool::Wait,
                        sender_thread_id,
                        receiver_thread_ids,
                        prompt: (),
                        agents_states: EmptyAgentStates {},
                        status,
                    },
            } if state == 2 => {
                last_item_was_answer = false;
                if id.is_empty()
                    || !ids.insert(id.clone())
                    || active_thread.as_deref() != Some(sender_thread_id.as_str())
                    || !receiver_thread_ids.is_empty()
                    || status != ToolStatus::InProgress
                    || !pending.is_empty()
                    || pending.insert(id, PendingTool::EmptyWait).is_some()
                {
                    return Err("invalid empty wait start");
                }
            }
            CodeEvent::ItemCompleted {
                item:
                    CodeItem::EmptyWait {
                        id,
                        tool: EmptyWaitTool::Wait,
                        sender_thread_id,
                        receiver_thread_ids,
                        prompt: (),
                        agents_states: EmptyAgentStates {},
                        status,
                    },
            } if state == 2 => {
                last_item_was_answer = false;
                if active_thread.as_deref() != Some(sender_thread_id.as_str())
                    || !receiver_thread_ids.is_empty()
                    || status != ToolStatus::Completed
                    || pending.remove(&id) != Some(PendingTool::EmptyWait)
                {
                    return Err("empty wait completion did not match its start");
                }
            }
            CodeEvent::Complete { usage: observed }
                if state == 2 && output.is_some() && last_item_was_answer && pending.is_empty() =>
            {
                if observed.cached_input_tokens > observed.input_tokens
                    || observed.reasoning_output_tokens > observed.output_tokens
                {
                    return Err("invalid usage");
                }
                usage = Some(observed);
                state = 3;
            }
            _ => return Err("ambiguous coding ordering or unsupported activity"),
        }
    }
    if state != 3 {
        return Err("missing coding terminal event");
    }
    Ok(CodeParsed {
        output: output.ok_or("missing coding answer")?,
        usage: usage.ok_or("missing coding usage")?,
        command_count,
        commands,
        file_change_count,
        tool_failed,
    })
}

fn validate_changes(changes: &[FileChange], workspace: &Path) -> Result<(), &'static str> {
    for change in changes {
        let path = Path::new(&change.path);
        let relative = path
            .strip_prefix(workspace)
            .map_err(|_| "file change escaped the coding workspace")?;
        if !path.is_absolute()
            || relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err("file change escaped the coding workspace");
        }
    }
    Ok(())
}
