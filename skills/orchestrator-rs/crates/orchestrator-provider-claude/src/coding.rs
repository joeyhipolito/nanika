//! Explicit first-use Claude coding contract.
//!
//! This executor is deliberately separate from [`super::ClaudeProvider`]. It
//! admits only Claude Code 2.1.269 in the standalone pilot, gives that process
//! five built-in file tools under `--restricted`, and validates their complete
//! wire lifecycle before accepting the provider result.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use orchestrator_exec::{
    AttemptEvidence, AttemptOutcome, ContractError, DispatchRequest, ExecutionContext,
    ExecutionRequest, FailureKind, MechanicalTermination, PartialWork, PhaseExecutor,
    ProcessPurpose, ProcessRequest, RuntimeCaps, RuntimeDescriptor, RuntimeFamily,
};
use serde_json::{Map, Value};

use super::{
    CLAUDE_EXECUTABLE_ID, CLAUDE_RUNTIME, ClaudeOutputError, ClaudeProtocolContract,
    ClaudeProviderConfig, ClaudeRequestError, EMPTY_MCP_CONFIG, ENTRYPOINT_KEY, ENTRYPOINT_VALUE,
    MAX_JSONL_LINE_BYTES, MAX_STDOUT_BYTES, OneShotOptions, ParsedOutput, build_worker_prompt,
    incomplete_with_failure, mechanical_termination, parse_one_shot_output_with_contract,
    reject_duplicate_object_keys,
};

const FILE_TOOLS_ARGUMENT: &str = "Read,Edit,Write,Glob,Grep";
const FILE_TOOLS: [&str; 5] = ["Edit", "Glob", "Grep", "Read", "Write"];
const PROTECTED_WRITE_COMPONENTS: [&str; 5] =
    [".git", ".claude", ".mcp.json", ".claude.json", "CLAUDE.md"];

/// Maximum number of turns admitted by the first-use Claude coding contract.
pub const FIRST_USE_PILOT_CLAUDE_MAX_TURNS: u64 = 5;

/// Non-enrolled Claude executor for one isolated first-use coding attempt.
///
/// The caller must bind execution to the snapshot directory through an
/// injected [`orchestrator_exec::ProcessService`]. This type cannot spawn a
/// process itself and is available only with `experimental-first-use-pilot`.
pub struct ClaudeCodingExecutor {
    config: ClaudeProviderConfig,
    runtime: RuntimeFamily,
}

impl ClaudeCodingExecutor {
    /// Creates the standalone coding executor with explicit provider configuration.
    pub fn new(config: ClaudeProviderConfig) -> Result<Self, ContractError> {
        Ok(Self {
            config,
            runtime: RuntimeFamily::parse(CLAUDE_RUNTIME)?,
        })
    }

    fn process_request(
        &self,
        request: &ExecutionRequest,
    ) -> Result<ProcessRequest, ClaudeRequestError> {
        let working_root = request.worker_dir();
        if request.target_dir() != Some(working_root)
            || request.resume_from().is_some()
            || request.hook_script().is_some()
            || request.model().is_empty()
            || request.max_turns() != FIRST_USE_PILOT_CLAUDE_MAX_TURNS
        {
            return Err(ClaudeRequestError::TargetDirectoryNotIsolated);
        }
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
        for argument in build_coding_arguments(&options) {
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
        process
            .with_environment(ENTRYPOINT_KEY, ENTRYPOINT_VALUE)
            .map_err(|_| ClaudeRequestError::ProcessContract)
    }
}

impl fmt::Debug for ClaudeCodingExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaudeCodingExecutor")
            .field("runtime", &self.runtime)
            .field("config", &self.config)
            .finish()
    }
}

impl PhaseExecutor for ClaudeCodingExecutor {
    fn execute(
        &self,
        dispatch: DispatchRequest<'_>,
        context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome {
        let request = dispatch.request();
        let process = match self.process_request(request) {
            Ok(process) => process,
            Err(_) => {
                return incomplete_with_failure(
                    MechanicalTermination::ContractViolation,
                    FailureKind::Protocol,
                    "Claude coding request could not satisfy the isolated process contract",
                    std::time::Duration::ZERO,
                );
            }
        };
        let receipt = match context.run_process(&process) {
            Ok(receipt) => receipt,
            Err(_) => {
                return AttemptOutcome::incomplete(
                    MechanicalTermination::SupervisorFailure,
                    None,
                    PartialWork::empty(),
                    std::time::Duration::ZERO,
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
        let parsed = match parse_coding_output(
            receipt.expose_stdout(),
            request.worker_dir(),
            request.model(),
            request.max_turns(),
        ) {
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
        let Some(cost) = parsed.cost else {
            return incomplete_with_failure(
                MechanicalTermination::ContractViolation,
                FailureKind::Protocol,
                "Claude coding result omitted authoritative usage or cost",
                receipt.elapsed(),
            );
        };
        let evidence = AttemptEvidence::new().with_cost(cost);
        AttemptOutcome::completed(parsed.output, evidence, receipt.elapsed()).unwrap_or_else(|_| {
            incomplete_with_failure(
                MechanicalTermination::ContractViolation,
                FailureKind::Protocol,
                "Claude coding output violated the completed-attempt contract",
                receipt.elapsed(),
            )
        })
    }

    fn descriptor(&self) -> Option<RuntimeDescriptor> {
        Some(RuntimeDescriptor::new(
            self.runtime.clone(),
            RuntimeCaps {
                tool_use: true,
                session_resume: false,
                streaming: false,
                cost_report: true,
                artifacts: false,
            },
        ))
    }
}

fn build_coding_arguments(options: &OneShotOptions<'_>) -> Vec<String> {
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
        "--restricted",
        "--setting-sources",
        "",
        "--disable-slash-commands",
        "--no-chrome",
        "--no-session-persistence",
        "--permission-mode",
        "acceptEdits",
        "--prompt-suggestions",
        "false",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    if !options.model.is_empty() {
        arguments.extend(["--model".to_owned(), options.model.to_owned()]);
    }
    if !options.effort.is_empty() {
        arguments.extend(["--effort".to_owned(), options.effort.to_owned()]);
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
    arguments.extend(["--tools".to_owned(), FILE_TOOLS_ARGUMENT.to_owned()]);
    arguments.extend([
        "--strict-mcp-config".to_owned(),
        "--mcp-config".to_owned(),
        EMPTY_MCP_CONFIG.to_owned(),
    ]);
    arguments
}

pub(crate) fn parse_coding_output(
    stdout: &[u8],
    working_root: &Path,
    requested_model: &str,
    max_turns: u64,
) -> Result<ParsedOutput, ClaudeOutputError> {
    let mut validator = CodingWireValidator::new(working_root, requested_model, max_turns)?;
    let mut normalized = Vec::with_capacity(stdout.len());
    let mut charged_bytes = 0_usize;
    let mut start = 0_usize;
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
        if line.len() >= MAX_JSONL_LINE_BYTES {
            return Err(ClaudeOutputError::LineLimit);
        }
        charged_bytes = charged_bytes
            .checked_add(line.len().saturating_add(1))
            .ok_or(ClaudeOutputError::CumulativeLimit)?;
        if charged_bytes > MAX_STDOUT_BYTES {
            return Err(ClaudeOutputError::CumulativeLimit);
        }
        if !line.is_empty() {
            let json = std::str::from_utf8(line).map_err(|_| ClaudeOutputError::Malformed)?;
            reject_duplicate_object_keys(json)?;
            if let Some(record) = validator.consume(json)? {
                normalized.extend_from_slice(record.as_bytes());
                normalized.push(b'\n');
            }
        }
        start = start.saturating_add(consumed);
    }
    let result_text = validator.finish()?;
    let mut parsed =
        parse_one_shot_output_with_contract(&normalized, ClaudeProtocolContract::first_use_pilot())
            .map_err(|error| {
                if error == ClaudeOutputError::ToolProtocol {
                    ClaudeOutputError::ForbiddenCodingActivity
                } else {
                    error
                }
            })?;
    if parsed.cost.is_none() {
        return Err(ClaudeOutputError::InvalidCost);
    }
    parsed.output = format!("{result_text}\n\n");
    Ok(parsed)
}

const MAX_TOOL_CALLS: usize = 128;
const MAX_TOOL_INPUT_BYTES: usize = 1024 * 1024;

struct ToolUse {
    completed: bool,
    mutates: bool,
    name: String,
    input: Value,
}

struct StreamTool {
    id: String,
    name: String,
    initial_input: Value,
    partial_json: String,
}

struct CodingWireValidator<'a> {
    working_root: &'a Path,
    requested_model: &'a str,
    max_turns: u64,
    session_id: Option<String>,
    initialized_model: Option<String>,
    tools: BTreeMap<String, ToolUse>,
    stream_tools: BTreeMap<u64, StreamTool>,
    closed_streams: BTreeMap<String, (String, Value)>,
    last_text: Option<String>,
    result_text: Option<String>,
    saw_init: bool,
    saw_result: bool,
    successful_mutation: bool,
}

impl<'a> CodingWireValidator<'a> {
    fn new(
        working_root: &'a Path,
        requested_model: &'a str,
        max_turns: u64,
    ) -> Result<Self, ClaudeOutputError> {
        if !working_root.is_absolute() || requested_model.is_empty() || max_turns == 0 {
            return Err(ClaudeOutputError::UnisolatedRuntime);
        }
        Ok(Self {
            working_root,
            requested_model,
            max_turns,
            session_id: None,
            initialized_model: None,
            tools: BTreeMap::new(),
            stream_tools: BTreeMap::new(),
            closed_streams: BTreeMap::new(),
            last_text: None,
            result_text: None,
            saw_init: false,
            saw_result: false,
            successful_mutation: false,
        })
    }

    fn consume(&mut self, json: &str) -> Result<Option<String>, ClaudeOutputError> {
        if self.saw_result {
            return Err(ClaudeOutputError::Malformed);
        }
        let mut value: Value =
            serde_json::from_str(json).map_err(|_| ClaudeOutputError::Malformed)?;
        let record = value.as_object_mut().ok_or(ClaudeOutputError::Malformed)?;
        if record
            .get("parent_tool_use_id")
            .is_some_and(|id| !id.is_null())
        {
            return Err(ClaudeOutputError::ForbiddenCodingActivity);
        }
        let record_type = required_string(record, "type")?;
        let is_init = record_type == "system"
            && record.get("subtype").and_then(Value::as_str) == Some("init");
        if !self.saw_init && !is_init {
            return Err(ClaudeOutputError::UnisolatedRuntime);
        }
        self.validate_session(record, is_init)?;
        match record_type.as_str() {
            "system" if is_init => self.consume_init(record)?,
            "system"
                if record.get("subtype").and_then(Value::as_str) == Some("permission_denied") =>
            {
                return Err(ClaudeOutputError::PermissionDenied);
            }
            "assistant" => self.consume_assistant(record)?,
            "stream_event" => self.consume_stream_event(record)?,
            "user" => {
                self.consume_tool_result(record)?;
                return Ok(None);
            }
            "result" => self.consume_result(record)?,
            _ => {}
        }
        serde_json::to_string(&value)
            .map(Some)
            .map_err(|_| ClaudeOutputError::Malformed)
    }

    fn validate_session(
        &mut self,
        record: &Map<String, Value>,
        is_init: bool,
    ) -> Result<(), ClaudeOutputError> {
        let observed = record.get("session_id").and_then(Value::as_str);
        if is_init {
            let session = observed
                .filter(|value| !value.is_empty())
                .ok_or(ClaudeOutputError::Malformed)?;
            self.session_id = Some(session.to_owned());
            return Ok(());
        }
        let observed = observed
            .filter(|value| !value.is_empty())
            .ok_or(ClaudeOutputError::Malformed)?;
        if self.session_id.as_deref() != Some(observed) {
            return Err(ClaudeOutputError::Malformed);
        }
        Ok(())
    }

    fn consume_init(&mut self, record: &mut Map<String, Value>) -> Result<(), ClaudeOutputError> {
        if self.saw_init {
            return Err(ClaudeOutputError::Malformed);
        }
        let cwd_value = required_string(record, "cwd")?;
        let cwd = Path::new(&cwd_value);
        if cwd != self.working_root {
            return Err(ClaudeOutputError::UnisolatedRuntime);
        }
        let tools = record
            .get("tools")
            .and_then(Value::as_array)
            .ok_or(ClaudeOutputError::Malformed)?;
        let observed = tools
            .iter()
            .map(|tool| tool.as_str().ok_or(ClaudeOutputError::Malformed))
            .collect::<Result<Vec<_>, _>>()?;
        if observed.as_slice() != FILE_TOOLS {
            return Err(ClaudeOutputError::UnisolatedRuntime);
        }
        let model = required_string(record, "model")?;
        if !model_agrees(self.requested_model, &model) {
            return Err(ClaudeOutputError::UnisolatedRuntime);
        }
        if record.get("permissionMode").and_then(Value::as_str) != Some("acceptEdits") {
            return Err(ClaudeOutputError::UnisolatedRuntime);
        }
        self.initialized_model = Some(model);
        self.saw_init = true;
        record.insert("tools".to_owned(), Value::Array(Vec::new()));
        record.insert(
            "permissionMode".to_owned(),
            Value::String("dontAsk".to_owned()),
        );
        Ok(())
    }

    fn consume_assistant(
        &mut self,
        record: &mut Map<String, Value>,
    ) -> Result<(), ClaudeOutputError> {
        let mut wire_inputs = match record.remove("wire_tool_inputs") {
            None => None,
            Some(Value::Object(inputs)) => Some(inputs),
            Some(_) => return Err(ClaudeOutputError::Malformed),
        };
        let content = if let Some(message) = record.get_mut("message") {
            let message = message
                .as_object_mut()
                .ok_or(ClaudeOutputError::Malformed)?;
            if let Some(model) = message.get("model").and_then(Value::as_str) {
                if self.initialized_model.as_deref() != Some(model) {
                    return Err(ClaudeOutputError::UnisolatedRuntime);
                }
            }
            message
                .get_mut("content")
                .and_then(Value::as_array_mut)
                .ok_or(ClaudeOutputError::Malformed)?
        } else {
            record
                .get_mut("content")
                .and_then(Value::as_array_mut)
                .ok_or(ClaudeOutputError::Malformed)?
        };
        let mut text = String::new();
        for block in content.iter_mut() {
            let object = block.as_object_mut().ok_or(ClaudeOutputError::Malformed)?;
            match object.get("type").and_then(Value::as_str) {
                Some("text") => text.push_str(required_string(object, "text")?.as_str()),
                Some("tool_use") => {
                    let (id, input) = self.consume_tool_use(object)?;
                    if let Some(inputs) = wire_inputs.as_mut() {
                        if inputs.remove(&id).as_ref().is_none_or(|wire| {
                            !tool_inputs_agree(object["name"].as_str().unwrap_or(""), wire, &input)
                        }) {
                            return Err(ClaudeOutputError::Malformed);
                        }
                    }
                    *block = empty_text_block();
                }
                Some("thinking" | "redacted_thinking") => {}
                Some(_) => return Err(ClaudeOutputError::UnknownProtocolValue),
                None => return Err(ClaudeOutputError::Malformed),
            }
        }
        if !text.is_empty() {
            self.last_text = Some(text);
        }
        if wire_inputs
            .as_ref()
            .is_some_and(|inputs| !inputs.is_empty())
        {
            return Err(ClaudeOutputError::Malformed);
        }
        Ok(())
    }

    fn consume_tool_use(
        &mut self,
        block: &Map<String, Value>,
    ) -> Result<(String, Value), ClaudeOutputError> {
        only_keys(block, &["type", "id", "name", "input", "caller"])?;
        let id = required_string(block, "id")?;
        if id.is_empty() || self.tools.contains_key(&id) || self.tools.len() >= MAX_TOOL_CALLS {
            return Err(ClaudeOutputError::Malformed);
        }
        let name = required_string(block, "name")?;
        let input_value = block
            .get("input")
            .cloned()
            .ok_or(ClaudeOutputError::Malformed)?;
        let input = input_value
            .as_object()
            .ok_or(ClaudeOutputError::Malformed)?;
        validate_caller(block.get("caller"))?;
        let mutates = validate_tool_input(&name, input, self.working_root)?;
        if let Some((stream_name, stream_input)) = self.closed_streams.get(&id) {
            if stream_name != &name || !tool_inputs_agree(&name, stream_input, &input_value) {
                return Err(ClaudeOutputError::Malformed);
            }
        }
        self.tools.insert(
            id.clone(),
            ToolUse {
                completed: false,
                mutates,
                name,
                input: input_value.clone(),
            },
        );
        Ok((id, input_value))
    }

    fn consume_stream_event(
        &mut self,
        record: &mut Map<String, Value>,
    ) -> Result<(), ClaudeOutputError> {
        let event = record
            .get_mut("event")
            .and_then(Value::as_object_mut)
            .ok_or(ClaudeOutputError::Malformed)?;
        let event_type = required_string(event, "type")?;
        match event_type.as_str() {
            "content_block_start" => {
                let index = optional_index(event)?;
                if self.stream_tools.contains_key(&index) {
                    return Err(ClaudeOutputError::Malformed);
                }
                let block = event
                    .get_mut("content_block")
                    .and_then(Value::as_object_mut)
                    .ok_or(ClaudeOutputError::Malformed)?;
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    only_keys(block, &["type", "id", "name", "input", "caller"])?;
                    let name = required_string(block, "name")?;
                    let id = required_string(block, "id")?;
                    if id.is_empty()
                        || !FILE_TOOLS.contains(&name.as_str())
                        || block.get("input").and_then(Value::as_object).is_none()
                    {
                        return Err(ClaudeOutputError::ForbiddenCodingActivity);
                    }
                    validate_caller(block.get("caller"))?;
                    if self.closed_streams.contains_key(&id)
                        || self.stream_tools.values().any(|tool| tool.id == id)
                        || self.closed_streams.len() + self.stream_tools.len() >= MAX_TOOL_CALLS
                    {
                        return Err(ClaudeOutputError::Malformed);
                    }
                    let initial_input = block
                        .get("input")
                        .cloned()
                        .ok_or(ClaudeOutputError::Malformed)?;
                    let input = initial_input
                        .as_object()
                        .ok_or(ClaudeOutputError::Malformed)?;
                    if !input.is_empty() {
                        validate_tool_input(&name, input, self.working_root)?;
                    }
                    self.stream_tools.insert(
                        index,
                        StreamTool {
                            id,
                            name,
                            initial_input,
                            partial_json: String::new(),
                        },
                    );
                    *block = empty_text_block()
                        .as_object()
                        .cloned()
                        .ok_or(ClaudeOutputError::Malformed)?;
                }
            }
            "content_block_delta" => {
                let index = optional_index(event)?;
                let delta = event
                    .get_mut("delta")
                    .and_then(Value::as_object_mut)
                    .ok_or(ClaudeOutputError::Malformed)?;
                if delta.get("type").and_then(Value::as_str) == Some("input_json_delta") {
                    only_keys(delta, &["type", "partial_json"])?;
                    let partial = required_string(delta, "partial_json")?;
                    let tool = self
                        .stream_tools
                        .get_mut(&index)
                        .ok_or(ClaudeOutputError::ForbiddenCodingActivity)?;
                    if tool.partial_json.len().saturating_add(partial.len()) > MAX_TOOL_INPUT_BYTES
                    {
                        return Err(ClaudeOutputError::CumulativeLimit);
                    }
                    tool.partial_json.push_str(&partial);
                    *delta = Map::from_iter([
                        ("type".to_owned(), Value::String("text_delta".to_owned())),
                        ("text".to_owned(), Value::String(String::new())),
                    ]);
                } else if self.stream_tools.contains_key(&index) {
                    return Err(ClaudeOutputError::Malformed);
                }
            }
            "content_block_stop" => {
                if let Some(index) = event.get("index").and_then(Value::as_u64) {
                    if let Some(tool) = self.stream_tools.remove(&index) {
                        let input = if tool.partial_json.is_empty() {
                            tool.initial_input
                        } else {
                            if !tool.initial_input.as_object().is_some_and(Map::is_empty) {
                                return Err(ClaudeOutputError::Malformed);
                            }
                            reject_duplicate_object_keys(&tool.partial_json)?;
                            serde_json::from_str::<Value>(&tool.partial_json)
                                .map_err(|_| ClaudeOutputError::Malformed)?
                        };
                        validate_tool_input(
                            &tool.name,
                            input.as_object().ok_or(ClaudeOutputError::Malformed)?,
                            self.working_root,
                        )?;
                        if let Some(authoritative) = self.tools.get(&tool.id) {
                            if authoritative.name != tool.name
                                || !tool_inputs_agree(&tool.name, &authoritative.input, &input)
                            {
                                return Err(ClaudeOutputError::Malformed);
                            }
                        }
                        self.closed_streams.insert(tool.id, (tool.name, input));
                    }
                }
            }
            "message_delta" => {
                let delta = event
                    .get_mut("delta")
                    .and_then(Value::as_object_mut)
                    .ok_or(ClaudeOutputError::Malformed)?;
                if delta.get("stop_reason").and_then(Value::as_str) == Some("tool_use") {
                    delta.insert("stop_reason".to_owned(), Value::Null);
                }
            }
            "message_start" | "message_stop" | "ping" | "error" => {}
            _ => return Err(ClaudeOutputError::UnknownProtocolValue),
        }
        Ok(())
    }

    fn consume_tool_result(
        &mut self,
        record: &Map<String, Value>,
    ) -> Result<(), ClaudeOutputError> {
        only_keys(
            record,
            &[
                "type",
                "message",
                "parent_tool_use_id",
                "session_id",
                "uuid",
                "timestamp",
                "tool_use_result",
                "tool_result_meta",
            ],
        )?;
        let message = record
            .get("message")
            .and_then(Value::as_object)
            .ok_or(ClaudeOutputError::Malformed)?;
        only_keys(message, &["role", "content"])?;
        if message.get("role").and_then(Value::as_str) != Some("user") {
            return Err(ClaudeOutputError::Malformed);
        }
        let content = message
            .get("content")
            .and_then(Value::as_array)
            .ok_or(ClaudeOutputError::Malformed)?;
        if content.len() != 1 {
            return Err(ClaudeOutputError::Malformed);
        }
        if record
            .get("parent_tool_use_id")
            .is_some_and(|id| !id.is_null())
        {
            return Err(ClaudeOutputError::ForbiddenCodingActivity);
        }
        for key in ["uuid", "timestamp"] {
            optional_string(record, key)?;
        }
        let tool_use_result = record
            .get("tool_use_result")
            .filter(|result| !result.is_null())
            .ok_or(ClaudeOutputError::Malformed)?;
        if record.get("tool_result_meta").is_some()
            || tool_use_result
                .as_str()
                .is_some_and(|result| result.starts_with("Error:"))
        {
            return Err(ClaudeOutputError::PermissionDenied);
        }
        for item in content {
            let item = item.as_object().ok_or(ClaudeOutputError::Malformed)?;
            only_keys(item, &["type", "tool_use_id", "content", "is_error"])?;
            if item.get("type").and_then(Value::as_str) != Some("tool_result")
                || item
                    .get("content")
                    .is_none_or(|content| !content.is_string())
            {
                return Err(ClaudeOutputError::Malformed);
            }
            match item.get("is_error") {
                Some(Value::Bool(true)) => return Err(ClaudeOutputError::PermissionDenied),
                Some(Value::Bool(false)) | None => {}
                Some(_) => return Err(ClaudeOutputError::Malformed),
            }
            let id = required_string(item, "tool_use_id")?;
            let tool = self
                .tools
                .get_mut(&id)
                .ok_or(ClaudeOutputError::Malformed)?;
            if tool.completed {
                return Err(ClaudeOutputError::Malformed);
            }
            if item
                .get("content")
                .and_then(Value::as_str)
                .is_some_and(|text| text.starts_with("Error:"))
            {
                return Err(ClaudeOutputError::PermissionDenied);
            }
            validate_tool_result(tool, tool_use_result, self.working_root)?;
            tool.completed = true;
            if tool.mutates {
                self.successful_mutation = true;
            }
        }
        Ok(())
    }

    fn consume_result(&mut self, record: &Map<String, Value>) -> Result<(), ClaudeOutputError> {
        if self.saw_result {
            return Err(ClaudeOutputError::Malformed);
        }
        if record
            .get("permission_denials")
            .and_then(Value::as_array)
            .is_some_and(|denials| !denials.is_empty())
        {
            return Err(ClaudeOutputError::PermissionDenied);
        }
        let turns = record
            .get("num_turns")
            .and_then(Value::as_u64)
            .ok_or(ClaudeOutputError::Malformed)?;
        if turns == 0 || turns > self.max_turns {
            return Err(ClaudeOutputError::ProviderReportedError);
        }
        let result = required_string(record, "result")?;
        if result.is_empty() || self.last_text.as_deref() != Some(result.as_str()) {
            return Err(ClaudeOutputError::EmptyOutput);
        }
        self.result_text = Some(result);
        self.saw_result = true;
        Ok(())
    }

    fn finish(self) -> Result<String, ClaudeOutputError> {
        if !self.saw_init {
            return Err(ClaudeOutputError::UnisolatedRuntime);
        }
        if !self.saw_result {
            return Err(ClaudeOutputError::MissingResult);
        }
        if !self.stream_tools.is_empty()
            || self.tools.values().any(|tool| !tool.completed)
            || self.closed_streams.iter().any(|(id, (name, input))| {
                self.tools.get(id).is_none_or(|tool| {
                    &tool.name != name || !tool_inputs_agree(name, &tool.input, input)
                })
            })
        {
            return Err(ClaudeOutputError::MissingResult);
        }
        if !self.successful_mutation {
            return Err(ClaudeOutputError::MissingMutation);
        }
        self.result_text.ok_or(ClaudeOutputError::EmptyOutput)
    }
}

// Claude 2.1.269 adds Edit's default to the authoritative input while the
// wire and streamed JSON can omit it. No other argument drift is equivalent.
fn tool_inputs_agree(name: &str, left: &Value, right: &Value) -> bool {
    if left == right {
        return true;
    }
    let (Some(left), Some(right)) = (left.as_object(), right.as_object()) else {
        return false;
    };
    if name != "Edit" {
        return false;
    }
    let replace_all = |input: &Map<String, Value>| match input.get("replace_all") {
        None => Some(false),
        Some(value) => value.as_bool(),
    };
    replace_all(left).is_some()
        && replace_all(left) == replace_all(right)
        && left
            .iter()
            .filter(|(key, _)| key.as_str() != "replace_all")
            .eq(right
                .iter()
                .filter(|(key, _)| key.as_str() != "replace_all"))
}

fn validate_tool_result(
    tool: &ToolUse,
    value: &Value,
    root: &Path,
) -> Result<(), ClaudeOutputError> {
    let result = value.as_object().ok_or(ClaudeOutputError::Malformed)?;
    let input = tool.input.as_object().ok_or(ClaudeOutputError::Malformed)?;
    match tool.name.as_str() {
        "Read" => {
            only_keys(result, &["type", "file"])?;
            if required_string(result, "type")? != "text" {
                return Err(ClaudeOutputError::UnknownProtocolValue);
            }
            let file = result
                .get("file")
                .and_then(Value::as_object)
                .ok_or(ClaudeOutputError::Malformed)?;
            only_keys(
                file,
                &["filePath", "content", "numLines", "startLine", "totalLines"],
            )?;
            matching_result_path(file, input, root, false)?;
            required_string(file, "content")?;
            require_numbers(file, &["numLines", "startLine", "totalLines"])?;
        }
        "Edit" => {
            only_keys(
                result,
                &[
                    "filePath",
                    "oldString",
                    "newString",
                    "originalFile",
                    "structuredPatch",
                    "userModified",
                    "replaceAll",
                ],
            )?;
            matching_result_path(result, input, root, true)?;
            if required_string(result, "oldString")? != required_string(input, "old_string")?
                || required_string(result, "newString")? != required_string(input, "new_string")?
                || result.get("replaceAll").and_then(Value::as_bool)
                    != Some(
                        input
                            .get("replace_all")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    )
            {
                return Err(ClaudeOutputError::Malformed);
            }
            required_string(result, "originalFile")?;
            validate_patch(result, true)?;
            if result.get("userModified").and_then(Value::as_bool) != Some(false) {
                return Err(ClaudeOutputError::Malformed);
            }
        }
        "Write" => {
            only_keys(
                result,
                &[
                    "type",
                    "filePath",
                    "content",
                    "structuredPatch",
                    "originalFile",
                    "userModified",
                ],
            )?;
            matching_result_path(result, input, root, true)?;
            if required_string(result, "content")? != required_string(input, "content")?
                || result.get("userModified").and_then(Value::as_bool) != Some(false)
            {
                return Err(ClaudeOutputError::Malformed);
            }
            match required_string(result, "type")?.as_str() {
                "create" if result.get("originalFile") == Some(&Value::Null) => {}
                "update" if result.get("originalFile").is_some_and(Value::is_string) => {}
                _ => return Err(ClaudeOutputError::UnknownProtocolValue),
            }
            validate_patch(result, false)?;
        }
        "Glob" => {
            only_keys(
                result,
                &[
                    "filenames",
                    "durationMs",
                    "numFiles",
                    "truncated",
                    "totalMatches",
                    "countIsComplete",
                ],
            )?;
            require_numbers(result, &["durationMs", "numFiles", "totalMatches"])?;
            if result.get("truncated").and_then(Value::as_bool) != Some(false)
                || result.get("countIsComplete").and_then(Value::as_bool) != Some(true)
            {
                return Err(ClaudeOutputError::Malformed);
            }
            validate_result_filenames(result, root)?;
        }
        "Grep" => {
            only_keys(
                result,
                &[
                    "mode",
                    "numFiles",
                    "filenames",
                    "content",
                    "numLines",
                    "totalLines",
                ],
            )?;
            if required_string(result, "mode")? != "content"
                || input.get("output_mode").and_then(Value::as_str) != Some("content")
            {
                return Err(ClaudeOutputError::UnknownProtocolValue);
            }
            required_string(result, "content")?;
            require_numbers(result, &["numFiles", "numLines", "totalLines"])?;
            validate_result_filenames(result, root)?;
        }
        _ => return Err(ClaudeOutputError::ForbiddenCodingActivity),
    }
    Ok(())
}

fn matching_result_path(
    result: &Map<String, Value>,
    input: &Map<String, Value>,
    root: &Path,
    write: bool,
) -> Result<(), ClaudeOutputError> {
    let reported = required_string(result, "filePath")?;
    let requested = required_string(input, "file_path")?;
    validate_tool_path(root, &reported, write)?;
    validate_tool_path(root, &requested, write)?;
    let reported: PathBuf = root.join(reported).components().collect();
    let requested: PathBuf = root.join(requested).components().collect();
    if reported != requested {
        return Err(ClaudeOutputError::ToolPath);
    }
    Ok(())
}

fn require_numbers(object: &Map<String, Value>, keys: &[&str]) -> Result<(), ClaudeOutputError> {
    if keys
        .iter()
        .all(|key| object.get(*key).is_some_and(Value::is_u64))
    {
        Ok(())
    } else {
        Err(ClaudeOutputError::Malformed)
    }
}

fn validate_patch(
    result: &Map<String, Value>,
    require_change: bool,
) -> Result<(), ClaudeOutputError> {
    let patch = result
        .get("structuredPatch")
        .and_then(Value::as_array)
        .ok_or(ClaudeOutputError::Malformed)?;
    if require_change && patch.is_empty() {
        return Err(ClaudeOutputError::Malformed);
    }
    for hunk in patch {
        let hunk = hunk.as_object().ok_or(ClaudeOutputError::Malformed)?;
        only_keys(
            hunk,
            &["oldStart", "oldLines", "newStart", "newLines", "lines"],
        )?;
        require_numbers(hunk, &["oldStart", "oldLines", "newStart", "newLines"])?;
        let lines = hunk
            .get("lines")
            .and_then(Value::as_array)
            .ok_or(ClaudeOutputError::Malformed)?;
        if !lines.iter().all(Value::is_string) {
            return Err(ClaudeOutputError::Malformed);
        }
    }
    Ok(())
}

fn validate_result_filenames(
    result: &Map<String, Value>,
    root: &Path,
) -> Result<(), ClaudeOutputError> {
    let files = result
        .get("filenames")
        .and_then(Value::as_array)
        .ok_or(ClaudeOutputError::Malformed)?;
    if result.get("numFiles").and_then(Value::as_u64) != Some(files.len() as u64) {
        return Err(ClaudeOutputError::Malformed);
    }
    for file in files {
        validate_tool_path(
            root,
            file.as_str().ok_or(ClaudeOutputError::Malformed)?,
            false,
        )?;
    }
    Ok(())
}

fn validate_tool_input(
    name: &str,
    input: &Map<String, Value>,
    working_root: &Path,
) -> Result<bool, ClaudeOutputError> {
    match name {
        "Read" => {
            only_keys(input, &["file_path", "offset", "limit", "pages"])?;
            validate_numeric_options(input, &["offset", "limit"])?;
            optional_string(input, "pages")?;
            validate_tool_path(working_root, &required_string(input, "file_path")?, false)?;
            Ok(false)
        }
        "Edit" => {
            only_keys(
                input,
                &["file_path", "old_string", "new_string", "replace_all"],
            )?;
            required_string(input, "old_string")?;
            required_string(input, "new_string")?;
            optional_bool(input, "replace_all")?;
            validate_tool_path(working_root, &required_string(input, "file_path")?, true)?;
            Ok(true)
        }
        "Write" => {
            only_keys(input, &["file_path", "content"])?;
            required_string(input, "content")?;
            validate_tool_path(working_root, &required_string(input, "file_path")?, true)?;
            Ok(true)
        }
        "Glob" => {
            only_keys(input, &["pattern", "path"])?;
            validate_search_glob(&required_string(input, "pattern")?)?;
            validate_optional_tool_path(input, "path", working_root)?;
            Ok(false)
        }
        "Grep" => {
            only_keys(
                input,
                &[
                    "pattern",
                    "path",
                    "glob",
                    "output_mode",
                    "-B",
                    "-A",
                    "-C",
                    "-n",
                    "-i",
                    "type",
                    "head_limit",
                    "offset",
                    "multiline",
                ],
            )?;
            if required_string(input, "pattern")?.is_empty() {
                return Err(ClaudeOutputError::Malformed);
            }
            optional_string(input, "output_mode")?;
            optional_string(input, "type")?;
            if let Some(glob) = input.get("glob").and_then(Value::as_str) {
                validate_search_glob(glob)?;
            } else {
                optional_string(input, "glob")?;
            }
            for key in ["-n", "-i", "multiline"] {
                optional_bool(input, key)?;
            }
            validate_numeric_options(input, &["-B", "-A", "-C", "head_limit", "offset"])?;
            validate_optional_tool_path(input, "path", working_root)?;
            Ok(false)
        }
        _ => Err(ClaudeOutputError::ForbiddenCodingActivity),
    }
}

fn validate_search_glob(pattern: &str) -> Result<(), ClaudeOutputError> {
    let path = Path::new(pattern);
    if pattern.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        Err(ClaudeOutputError::ToolPath)
    } else {
        Ok(())
    }
}

fn validate_tool_path(
    working_root: &Path,
    raw: &str,
    write: bool,
) -> Result<(), ClaudeOutputError> {
    if raw.is_empty() || raw.contains('\0') {
        return Err(ClaudeOutputError::Malformed);
    }
    let path = Path::new(raw);
    let mut resolved = if path.is_absolute() {
        PathBuf::new()
    } else {
        working_root.to_path_buf()
    };
    for component in path.components() {
        match component {
            Component::RootDir if path.is_absolute() => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            Component::Normal(part) => resolved.push(part),
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err(ClaudeOutputError::ToolPath);
            }
        }
    }
    if !resolved.starts_with(working_root) {
        return Err(ClaudeOutputError::ToolPath);
    }
    if write
        && resolved
            .strip_prefix(working_root)
            .map_err(|_| ClaudeOutputError::ToolPath)?
            .components()
            .filter_map(|component| match component {
                Component::Normal(part) => part.to_str(),
                _ => None,
            })
            .any(|part| PROTECTED_WRITE_COMPONENTS.contains(&part))
    {
        return Err(ClaudeOutputError::ToolPath);
    }
    Ok(())
}

fn validate_optional_tool_path(
    input: &Map<String, Value>,
    key: &str,
    working_root: &Path,
) -> Result<(), ClaudeOutputError> {
    match input.get(key) {
        None => Ok(()),
        Some(Value::String(path)) => validate_tool_path(working_root, path, false),
        Some(_) => Err(ClaudeOutputError::Malformed),
    }
}

fn model_agrees(requested: &str, initialized: &str) -> bool {
    requested == initialized
        || matches!(requested, "haiku" | "sonnet" | "opus")
            && initialized.starts_with(&format!("claude-{requested}-"))
}

fn validate_caller(caller: Option<&Value>) -> Result<(), ClaudeOutputError> {
    let caller = caller
        .and_then(Value::as_object)
        .ok_or(ClaudeOutputError::Malformed)?;
    only_keys(caller, &["type"])?;
    if caller.get("type").and_then(Value::as_str) == Some("direct") {
        Ok(())
    } else {
        Err(ClaudeOutputError::ForbiddenCodingActivity)
    }
}

fn empty_text_block() -> Value {
    Value::Object(Map::from_iter([
        ("type".to_owned(), Value::String("text".to_owned())),
        ("text".to_owned(), Value::String(String::new())),
    ]))
}

fn optional_index(event: &Map<String, Value>) -> Result<u64, ClaudeOutputError> {
    event
        .get("index")
        .and_then(Value::as_u64)
        .ok_or(ClaudeOutputError::Malformed)
}

fn required_string(object: &Map<String, Value>, key: &str) -> Result<String, ClaudeOutputError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(ClaudeOutputError::Malformed)
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Result<(), ClaudeOutputError> {
    if object.get(key).is_none_or(Value::is_string) {
        Ok(())
    } else {
        Err(ClaudeOutputError::Malformed)
    }
}

fn optional_bool(object: &Map<String, Value>, key: &str) -> Result<(), ClaudeOutputError> {
    if object.get(key).is_none_or(Value::is_boolean) {
        Ok(())
    } else {
        Err(ClaudeOutputError::Malformed)
    }
}

fn validate_numeric_options(
    object: &Map<String, Value>,
    keys: &[&str],
) -> Result<(), ClaudeOutputError> {
    if keys
        .iter()
        .all(|key| object.get(*key).is_none_or(Value::is_u64))
    {
        Ok(())
    } else {
        Err(ClaudeOutputError::Malformed)
    }
}

fn only_keys(object: &Map<String, Value>, allowed: &[&str]) -> Result<(), ClaudeOutputError> {
    if object.keys().all(|key| allowed.contains(&key.as_str())) {
        Ok(())
    } else {
        Err(ClaudeOutputError::Malformed)
    }
}

#[cfg(test)]
#[path = "coding_tests.rs"]
mod tests;
