use std::collections::{BTreeMap, VecDeque};

use serde_json::Value;

use crate::observe::safe_text;

pub(crate) const MAX_RECORDS: usize = 2_048;
pub(crate) const MAX_TOOL_CARDS: usize = 512;
pub(crate) const MAX_PHASES: usize = 256;
const MAX_DETAIL_BYTES: usize = 32 * 1024;
const MAX_SEARCH_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Focus {
    Phases,
    Activity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Filter {
    All,
    Tools,
    Messages,
    Problems,
}

impl Filter {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::All => Self::Tools,
            Self::Tools => Self::Messages,
            Self::Messages => Self::Problems,
            Self::Problems => Self::All,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Tools => "tools",
            Self::Messages => "messages",
            Self::Problems => "problems",
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ToolKey {
    phase: Option<String>,
    session: Option<String>,
    id: String,
    family: &'static str,
}

#[derive(Clone, Debug)]
pub(crate) struct Activity {
    pub(crate) sequence: u64,
    pub(crate) kind: String,
    pub(crate) phase: Option<String>,
    phase_key: Option<String>,
    pub(crate) session: Option<String>,
    pub(crate) summary: String,
    start_summary: Option<String>,
    pub(crate) detail: String,
    pub(crate) complete: bool,
    pub(crate) tool: bool,
    pub(crate) message: bool,
    pub(crate) problem: bool,
    pub(crate) unresolved: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Phase {
    pub(crate) id: String,
    key: String,
    pub(crate) route: Option<String>,
    pub(crate) owner_status: Option<String>,
}

pub(crate) struct ViewModel {
    records: VecDeque<Activity>,
    phases: Vec<Phase>,
    phase_lookup: BTreeMap<String, usize>,
    mission_status: Option<String>,
    tools: BTreeMap<ToolKey, u64>,
    pub(crate) focus: Focus,
    pub(crate) selected_phase: usize,
    selected_sequence: Option<u64>,
    pub(crate) detail_open: bool,
    pub(crate) detail_scroll: usize,
    pub(crate) filter: Filter,
    pub(crate) search: String,
    pub(crate) editing_search: bool,
    pub(crate) following: bool,
    pub(crate) replaying: bool,
    pub(crate) replay_complete: bool,
    pub(crate) source_error: Option<String>,
    pub(crate) evicted_records: u64,
    pub(crate) evicted_tools: u64,
    pub(crate) omitted_phase_observations: u64,
    pub(crate) usage_observed: bool,
    pub(crate) dependencies_observed: bool,
}

impl ViewModel {
    pub(crate) fn new(following: bool) -> Self {
        Self {
            records: VecDeque::new(),
            phases: Vec::new(),
            phase_lookup: BTreeMap::new(),
            mission_status: None,
            tools: BTreeMap::new(),
            focus: Focus::Activity,
            selected_phase: 0,
            selected_sequence: None,
            detail_open: false,
            detail_scroll: 0,
            filter: Filter::All,
            search: String::new(),
            editing_search: false,
            following,
            replaying: true,
            replay_complete: false,
            source_error: None,
            evicted_records: 0,
            evicted_tools: 0,
            omitted_phase_observations: 0,
            usage_observed: false,
            dependencies_observed: false,
        }
    }

    pub(crate) fn ingest(&mut self, value: Value) {
        let sequence = value.get("sequence").and_then(Value::as_u64).unwrap_or(0);
        let kind = text(&value, "kind").unwrap_or("unknown").to_owned();
        if kind == "owner_journal" && value.get("phase_id").is_none_or(Value::is_null) {
            match value.pointer("/body/detail/event").and_then(Value::as_str) {
                Some("mission_started") => self.mission_status = None,
                Some("mission_terminal") => {
                    self.mission_status = value
                        .pointer("/body/detail/status")
                        .and_then(Value::as_str)
                        .map(safe_text)
                }
                Some("cancellation_requested") => {
                    self.mission_status = Some("cancellation requested".into())
                }
                _ => {}
            }
        }
        self.usage_observed |= kind == "usage";
        if matches!(kind.as_str(), "heartbeat" | "provider_chunk") {
            return;
        }
        self.dependencies_observed |= kind == "owner_journal"
            && value
                .pointer("/body/detail/dependencies")
                .is_some_and(Value::is_array);
        let phase_key = text(&value, "phase_id").map(str::to_owned);
        let session_key = value
            .pointer("/provider/session_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.observe_phase(phase_key.as_deref(), &kind, &value);
        let phase = phase_key.as_deref().map(safe_text);
        let session = session_key.as_deref().map(safe_text);

        let tool_family = tool_family(&kind);
        let tool_id = value
            .pointer("/provider/item_id")
            .and_then(Value::as_str)
            .or_else(|| value.pointer("/provider/call_id").and_then(Value::as_str))
            .filter(|id| !id.is_empty());
        let key = match (
            tool_family,
            tool_id,
            phase_key.as_ref(),
            session_key.as_ref(),
        ) {
            (Some(family), Some(id), Some(phase), Some(session)) => Some(ToolKey {
                phase: Some(phase.clone()),
                session: Some(session.clone()),
                id: id.to_owned(),
                family,
            }),
            _ => None,
        };
        let starts_tool = is_tool_start(&kind)
            || kind == "file_change" && event_action(&value) == Some("item.started");
        let completes_tool = is_tool_result(&kind)
            || kind == "file_change" && event_action(&value) == Some("item.completed");
        let event_detail =
            annotated_detail(&kind, &value, tool_family.is_some() && tool_id.is_none());

        if !completes_tool {
            if let Some(key) = &key {
                if let Some(existing) = self.tools.get(key).copied() {
                    if let Some(card) = self.records.iter_mut().find(|row| row.sequence == existing)
                    {
                        card.detail = combine_detail(&card.detail, &event_detail);
                        card.complete &= value
                            .get("complete")
                            .and_then(Value::as_bool)
                            .unwrap_or(true);
                        self.repair_selection();
                        return;
                    }
                }
            }
        }

        if completes_tool {
            if let Some(key) = &key {
                if let Some(existing) = self.tools.get(key).copied() {
                    if let Some(card) = self.records.iter_mut().find(|row| row.sequence == existing)
                    {
                        let outcome = result_summary(&kind, &value, card.start_summary.is_none());
                        card.summary = match &card.start_summary {
                            Some(identity) => one_line(&format!("{outcome} · {identity}"), 240),
                            None => one_line(&outcome, 240),
                        };
                        card.detail = combine_detail(&card.detail, &event_detail);
                        card.complete &= value
                            .get("complete")
                            .and_then(Value::as_bool)
                            .unwrap_or(true);
                        card.problem |= result_failed(&value);
                        card.unresolved = false;
                        self.repair_selection();
                        return;
                    }
                }
            }
        }

        let missing_start = completes_tool;
        let unresolved = starts_tool;
        let activity = Activity {
            sequence,
            kind: kind.clone(),
            phase,
            phase_key,
            session,
            summary: if missing_start {
                result_summary(&kind, &value, true)
            } else {
                summary(&kind, &value)
            },
            start_summary: starts_tool.then(|| summary(&kind, &value)),
            detail: event_detail,
            complete: value
                .get("complete")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            tool: tool_family.is_some(),
            message: matches!(kind.as_str(), "assistant_message" | "user_message"),
            problem: is_problem(&kind) || result_failed(&value),
            unresolved,
        };
        self.insert(activity, key);
    }

    fn observe_phase(&mut self, phase: Option<&str>, kind: &str, value: &Value) {
        let Some(id) = phase.filter(|id| !id.is_empty()) else {
            return;
        };
        let index = if let Some(index) = self.phase_lookup.get(id).copied() {
            index
        } else {
            if self.phases.len() >= MAX_PHASES {
                self.omitted_phase_observations = self.omitted_phase_observations.saturating_add(1);
                return;
            }
            let index = self.phases.len();
            self.phases.push(Phase {
                id: safe_text(id),
                key: id.to_owned(),
                route: None,
                owner_status: None,
            });
            self.phase_lookup.insert(id.to_owned(), index);
            index
        };
        if kind == "owner_route" {
            self.phases[index].route = Some(route_summary(value));
        }
        if matches!(kind, "owner_journal" | "owner_stage" | "supervisor_status") {
            self.phases[index].owner_status =
                find_string(value, &["status", "state", "stage"]).map(|status| safe_text(&status));
        }
    }

    fn insert(&mut self, activity: Activity, key: Option<ToolKey>) {
        if activity.tool && self.records.iter().filter(|row| row.tool).count() >= MAX_TOOL_CARDS {
            if let Some(sequence) = self
                .records
                .iter()
                .find(|row| row.tool)
                .map(|row| row.sequence)
            {
                self.remove_sequence(sequence);
                self.evicted_tools = self.evicted_tools.saturating_add(1);
            }
        }
        while self.records.len() >= MAX_RECORDS {
            if let Some(row) = self.records.pop_front() {
                self.remove_tool_sequence(row.sequence);
                self.evicted_records = self.evicted_records.saturating_add(1);
            }
        }
        let sequence = activity.sequence;
        self.records.push_back(activity);
        if let Some(key) = key {
            self.tools.insert(key, sequence);
        }
        if self.following || self.selected_sequence.is_none() {
            self.selected_sequence = self.visible_sequences().last().copied();
        } else {
            self.repair_selection();
        }
    }

    fn remove_sequence(&mut self, sequence: u64) {
        if let Some(index) = self.records.iter().position(|row| row.sequence == sequence) {
            self.records.remove(index);
        }
        self.remove_tool_sequence(sequence);
        self.repair_selection();
    }

    fn remove_tool_sequence(&mut self, sequence: u64) {
        self.tools.retain(|_, value| *value != sequence);
    }

    pub(crate) fn phases(&self) -> &[Phase] {
        &self.phases
    }

    pub(crate) fn retained_count(&self) -> usize {
        self.records.len()
    }

    pub(crate) fn provider_status(&self) -> &'static str {
        for row in self.records.iter().rev() {
            if row.kind == "provider_error" {
                return "error observed";
            }
            if matches!(
                row.kind.as_str(),
                "provider_session"
                    | "provider_turn"
                    | "assistant_message"
                    | "command_start"
                    | "command_result"
                    | "mcp_tool_start"
                    | "mcp_tool_result"
                    | "tool_start"
                    | "tool_result"
            ) {
                return "activity observed (not liveness)";
            }
        }
        "unavailable"
    }

    pub(crate) fn owner_status(&self) -> &str {
        if self.selected_phase != 0 {
            return self
                .phases
                .get(self.selected_phase - 1)
                .and_then(|phase| phase.owner_status.as_deref())
                .unwrap_or("unavailable");
        }
        if let Some(status) = self.mission_status.as_deref() {
            return status;
        }
        self.phases
            .iter()
            .rev()
            .find_map(|phase| phase.owner_status.as_deref())
            .unwrap_or("unavailable")
    }

    pub(crate) fn visible(&self) -> Vec<&Activity> {
        self.records
            .iter()
            .filter(|row| self.matches(row))
            .collect()
    }

    pub(crate) fn selected(&self) -> Option<&Activity> {
        let selected = self.selected_sequence?;
        self.records
            .iter()
            .find(|row| row.sequence == selected && self.matches(row))
    }

    pub(crate) fn move_selection(&mut self, delta: isize) {
        if self.focus == Focus::Phases {
            self.move_phase(delta);
            return;
        }
        let visible = self.visible_sequences();
        if visible.is_empty() {
            self.selected_sequence = None;
            return;
        }
        let current = self
            .selected_sequence
            .and_then(|selected| visible.iter().position(|value| *value == selected))
            .unwrap_or(0);
        let next = current.saturating_add_signed(delta).min(visible.len() - 1);
        self.selected_sequence = Some(visible[next]);
        self.following = false;
        self.detail_scroll = 0;
    }

    fn move_phase(&mut self, delta: isize) {
        let count = self.phases.len().saturating_add(1);
        self.selected_phase = self
            .selected_phase
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
        self.repair_selection();
    }

    pub(crate) fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.repair_selection();
    }

    pub(crate) fn push_search(&mut self, character: char) {
        if !character.is_control()
            && self.search.len().saturating_add(character.len_utf8()) <= MAX_SEARCH_BYTES
        {
            self.search.push(character);
            self.repair_selection();
        }
    }

    pub(crate) fn pop_search(&mut self) {
        self.search.pop();
        self.repair_selection();
    }

    pub(crate) fn select_last(&mut self) {
        self.selected_sequence = self.visible_sequences().last().copied();
    }

    fn visible_sequences(&self) -> Vec<u64> {
        self.records
            .iter()
            .filter(|row| self.matches(row))
            .map(|row| row.sequence)
            .collect()
    }

    fn repair_selection(&mut self) {
        if self.selected().is_none() {
            self.selected_sequence = self.visible_sequences().first().copied();
        }
        self.detail_scroll = 0;
    }

    fn matches(&self, row: &Activity) -> bool {
        if self.selected_phase != 0 {
            let Some(phase) = self.phases.get(self.selected_phase - 1) else {
                return false;
            };
            if row.phase_key.as_deref() != Some(phase.key.as_str()) {
                return false;
            }
        }
        let category = match self.filter {
            Filter::All => true,
            Filter::Tools => row.tool,
            Filter::Messages => row.message,
            Filter::Problems => row.problem || row.unresolved,
        };
        if !category {
            return false;
        }
        let query = self.search.to_ascii_lowercase();
        query.is_empty()
            || row.summary.to_ascii_lowercase().contains(&query)
            || row.detail.to_ascii_lowercase().contains(&query)
    }
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn tool_family(kind: &str) -> Option<&'static str> {
    match kind {
        "command_start" | "command_update" | "command_result" => Some("command"),
        "mcp_tool_start" | "mcp_tool_update" | "mcp_tool_result" => Some("mcp"),
        "tool_start" | "tool_input" | "tool_input_delta" | "tool_block_closed" | "tool_result" => {
            Some("tool")
        }
        "file_change" => Some("file"),
        _ => None,
    }
}

fn is_tool_start(kind: &str) -> bool {
    matches!(kind, "command_start" | "mcp_tool_start" | "tool_start")
}

fn is_tool_result(kind: &str) -> bool {
    matches!(kind, "command_result" | "mcp_tool_result" | "tool_result")
}

fn is_problem(kind: &str) -> bool {
    matches!(
        kind,
        "provider_error" | "supervisor_error" | "malformed" | "truncated" | "incomplete" | "lost"
    )
}

fn event_action(value: &Value) -> Option<&str> {
    value.pointer("/body/nested/type").and_then(Value::as_str)
}

fn result_failed(value: &Value) -> bool {
    value
        .pointer("/body/nested/item/exit_code")
        .and_then(Value::as_i64)
        .is_some_and(|code| code != 0)
        || value
            .pointer("/body/nested/is_error")
            .and_then(Value::as_bool)
            == Some(true)
        || value
            .pointer("/body/nested/item/error")
            .is_some_and(|error| !error.is_null())
        || value
            .pointer("/body/nested/item/status")
            .and_then(Value::as_str)
            .is_some_and(|status| matches!(status, "failed" | "error" | "cancelled"))
}

fn summary(kind: &str, value: &Value) -> String {
    let label = match kind {
        "command_start" => "Command started",
        "mcp_tool_start" => "MCP tool started",
        "tool_start" => "Tool started",
        "file_change" => "File change",
        "assistant_message" => "Assistant",
        "user_message" => "User",
        "provider_error" => "Provider error",
        "supervisor_error" => "Supervisor failure",
        "supervisor_cancelled" => "Cancellation",
        "owner_route" => "Route",
        "usage" => "Usage",
        "lost" => "Lost observations",
        "truncated" => "Truncated observation",
        "incomplete" => "Incomplete observation",
        other => other,
    };
    let content = find_string(
        value,
        &[
            "command", "name", "text", "message", "reason", "status", "path",
        ],
    );
    match content {
        Some(content) if !content.is_empty() => format!("{label}: {}", one_line(&content, 160)),
        _ => label.to_owned(),
    }
}

fn result_summary(kind: &str, value: &Value, missing_start: bool) -> String {
    let outcome = if result_failed(value) {
        "Tool failed"
    } else if result_succeeded(value) {
        "Tool completed"
    } else {
        "Tool outcome unknown"
    };
    let start = if missing_start {
        " (start unavailable)"
    } else {
        ""
    };
    let exit = value
        .pointer("/body/nested/item/exit_code")
        .and_then(Value::as_i64)
        .map(|code| format!(" · exit {code}"))
        .unwrap_or_default();
    format!("{outcome}{start} · {kind}{exit}")
}

fn result_succeeded(value: &Value) -> bool {
    if value.get("complete").and_then(Value::as_bool) == Some(false) {
        return false;
    }
    value
        .pointer("/body/nested/item/exit_code")
        .and_then(Value::as_i64)
        == Some(0)
        || value
            .pointer("/body/nested/item/status")
            .and_then(Value::as_str)
            .is_some_and(|status| matches!(status, "success" | "succeeded"))
}

fn route_summary(value: &Value) -> String {
    let runtime =
        find_string(value, &["runtime", "provider"]).unwrap_or_else(|| "unavailable".into());
    let model = find_string(value, &["model"]).unwrap_or_else(|| "unavailable".into());
    let effort =
        find_string(value, &["effort", "reasoning_effort"]).unwrap_or_else(|| "unavailable".into());
    one_line(&format!("{runtime} · model {model} · effort {effort}"), 240)
}

fn detail_text(value: &Value) -> String {
    let body = value.get("body").unwrap_or(value);
    let rendered =
        serde_json::to_string_pretty(body).unwrap_or_else(|_| "detail unavailable".into());
    let provider_timestamp = value
        .get("provider_timestamp")
        .filter(|timestamp| !timestamp.is_null())
        .map(Value::to_string)
        .unwrap_or_else(|| "unavailable".into());
    let observation_timestamp = value
        .get("observation_time_unix_ms")
        .filter(|timestamp| !timestamp.is_null())
        .map(Value::to_string)
        .unwrap_or_else(|| "unavailable".into());
    let safe = format!(
        "provider timestamp: {}\nlocal replay observation time (not provider activity): {}\n{}",
        safe_text(&provider_timestamp),
        safe_text(&observation_timestamp),
        rendered
            .lines()
            .map(safe_text)
            .collect::<Vec<_>>()
            .join("\n")
    );
    if safe.len() <= MAX_DETAIL_BYTES {
        safe
    } else {
        let end = floor_char_boundary(&safe, MAX_DETAIL_BYTES);
        format!(
            "{}\n…[detail truncated at {MAX_DETAIL_BYTES} bytes]",
            &safe[..end]
        )
    }
}

fn annotated_detail(kind: &str, value: &Value, missing_id: bool) -> String {
    let mut detail = detail_text(value);
    if missing_id {
        detail = combine_detail(&detail, "correlation id: unavailable");
    }
    if kind == "file_change" && !contains_key(value, &["diff", "patch"]) {
        detail = combine_detail(&detail, "diff: unavailable");
    }
    if matches!(kind, "command_result" | "mcp_tool_result" | "tool_result") {
        if !contains_key(value, &["exit_code"]) {
            detail = combine_detail(&detail, "exit: unavailable");
        }
        if !contains_key(value, &["aggregated_output", "output", "result", "content"]) {
            detail = combine_detail(&detail, "result/output: unavailable");
        }
    }
    detail
}

fn combine_detail(first: &str, second: &str) -> String {
    if first.len().saturating_add(second.len()).saturating_add(2) <= MAX_DETAIL_BYTES {
        return format!("{first}\n\n{second}");
    }
    let budget = MAX_DETAIL_BYTES / 2 - 64;
    format!(
        "{}\n…[earlier detail truncated]\n\n{}\n…[latest detail preview bounded]",
        &first[..floor_char_boundary(first, budget)],
        &second[..floor_char_boundary(second, budget)]
    )
}

fn find_string(value: &Value, names: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for name in names {
                if let Some(found) = map.get(*name) {
                    if let Some(text) = found.as_str() {
                        return Some(text.to_owned());
                    }
                }
            }
            map.values().find_map(|value| find_string(value, names))
        }
        Value::Array(values) => values.iter().find_map(|value| find_string(value, names)),
        _ => None,
    }
}

fn contains_key(value: &Value, names: &[&str]) -> bool {
    match value {
        Value::Object(map) => {
            names
                .iter()
                .any(|name| map.get(*name).is_some_and(|value| !value.is_null()))
                || map.values().any(|value| contains_key(value, names))
        }
        Value::Array(values) => values.iter().any(|value| contains_key(value, names)),
        _ => false,
    }
}

fn one_line(text: &str, maximum: usize) -> String {
    let safe = safe_text(text)
        .replace("\\u{000a}", " ")
        .replace("\\u{000d}", " ");
    if safe.len() <= maximum {
        safe
    } else {
        let end = floor_char_boundary(&safe, maximum);
        format!("{}…", &safe[..end])
    }
}

fn floor_char_boundary(text: &str, maximum: usize) -> usize {
    let mut index = maximum.min(text.len());
    while !text.is_char_boundary(index) {
        index = index.saturating_sub(1);
    }
    index
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn row(sequence: u64, kind: &str, phase: &str, session: &str, id: &str) -> Value {
        json!({"sequence":sequence,"kind":kind,"phase_id":phase,
            "provider":{"session_id":session,"item_id":id},"complete":true,
            "body":{"nested":{"item":{"id":id,"type":"command_execution","command":"echo ok"}}}})
    }

    #[test]
    fn mission_failure_is_not_hidden_by_a_later_skipped_phase() {
        let mut model = ViewModel::new(false);
        model.ingest(json!({"sequence":1,"kind":"owner_journal","phase_id":"verify","body":{"detail":{"event":"phase_skipped_observation","status":"skipped"}}}));
        model.ingest(json!({"sequence":2,"kind":"owner_journal","phase_id":null,"body":{"detail":{"event":"mission_terminal","status":"failed"}}}));
        assert_eq!(model.owner_status(), "failed");
        model.selected_phase = 1;
        assert_eq!(model.owner_status(), "skipped");
    }

    #[test]
    fn repeated_claude_start_representations_keep_one_call_and_one_result() {
        let mut model = ViewModel::new(false);
        for (sequence, kind) in [
            (1, "tool_start"),
            (2, "tool_input"),
            (3, "tool_block_closed"),
            (4, "tool_start"),
            (5, "tool_result"),
        ] {
            model.ingest(json!({"sequence":sequence,"kind":kind,"phase_id":"a","provider":{"session_id":"s","item_id":null,"call_id":"call"},"body":{"nested":{"tool_use_id":"call","name":"example"}}}));
        }
        assert_eq!(model.records.len(), 1);
        assert_eq!(model.records[0].sequence, 1);
        assert!(!model.records[0].unresolved);
    }

    #[test]
    fn completed_results_repair_filtered_selection_and_preserve_updates() {
        let mut model = ViewModel::new(false);
        model.ingest(row(1, "command_start", "a", "s", "first"));
        model.ingest(row(2, "command_start", "a", "s", "second"));
        model.set_filter(Filter::Problems);
        assert_eq!(model.selected().map(|r| r.sequence), Some(1));
        model.ingest(row(3, "command_update", "a", "s", "first"));
        assert_eq!(model.records.len(), 2);
        model.ingest(row(4, "command_result", "a", "s", "first"));
        assert_eq!(model.selected().map(|r| r.sequence), Some(2));
        assert!(!model.records[0].unresolved);
    }

    #[test]
    fn claude_error_result_is_a_problem_but_payload_status_is_not_execution_status() {
        let mut model = ViewModel::new(false);
        for (sequence, kind) in [
            (1, "tool_start"),
            (2, "tool_input_delta"),
            (3, "tool_block_closed"),
            (4, "tool_result"),
        ] {
            model.ingest(json!({"sequence":sequence,"kind":kind,"phase_id":"a","provider":{"session_id":"s","item_id":null,"call_id":"call"},"body":{"nested":{"tool_use_id":"call","is_error":kind=="tool_result","content":"failure"}}}));
            if kind == "tool_block_closed" {
                assert!(model.records[0].unresolved);
            }
        }
        assert_eq!(model.records.len(), 1);
        assert!(model.records[0].problem);
        assert!(!model.records[0].unresolved);
        assert!(!result_failed(
            &json!({"body":{"nested":{"type":"tool_result","content":{"status":"failed"}}}})
        ));
    }

    #[test]
    fn anonymous_tool_cards_and_phase_history_obey_their_independent_caps() {
        let mut model = ViewModel::new(false);
        for sequence in 0..(MAX_TOOL_CARDS + 30) {
            model.ingest(json!({"sequence":sequence,"kind":"tool_start","phase_id":format!("phase-{sequence}"),"provider":{"item_id":null,"call_id":null}}));
        }
        assert_eq!(
            model.records.iter().filter(|r| r.tool).count(),
            MAX_TOOL_CARDS
        );
        assert_eq!(model.phases.len(), MAX_PHASES);
        assert!(model.evicted_tools > 0 && model.omitted_phase_observations > 0);
    }

    #[test]
    fn large_start_detail_cannot_hide_the_latest_tool_result() {
        let combined = combine_detail(&"a".repeat(MAX_DETAIL_BYTES), "LATEST_RESULT_VISIBLE");
        assert!(combined.contains("LATEST_RESULT_VISIBLE"));
        assert!(combined.len() <= MAX_DETAIL_BYTES);
    }

    #[test]
    fn tool_results_pair_only_with_same_phase_and_session() {
        let mut model = ViewModel::new(false);
        model.ingest(row(1, "command_start", "a", "one", "same"));
        model.ingest(row(2, "command_start", "b", "two", "same"));
        model.ingest(row(3, "command_result", "a", "one", "same"));
        assert!(!model.records[0].unresolved);
        assert!(model.records[1].unresolved);
    }

    #[test]
    fn result_without_start_is_explicit() {
        let mut model = ViewModel::new(false);
        model.ingest(row(1, "command_result", "a", "one", "missing"));
        assert!(model.records[0].summary.contains("start unavailable"));
    }

    #[test]
    fn completed_long_command_keeps_result_visible_at_card_start() {
        let mut model = ViewModel::new(false);
        model.ingest(json!({"sequence":1,"kind":"command_start","phase_id":"a",
            "provider":{"session_id":"one","item_id":"long"},
            "body":{"nested":{"item":{"command":"echo a very long command that exceeds a narrow inspector pane"}}}}));
        model.ingest(json!({"sequence":2,"kind":"command_result","phase_id":"a",
            "provider":{"session_id":"one","item_id":"long"},
            "body":{"nested":{"item":{"exit_code":0}}}}));
        let row = &model.records[0];
        assert!(row.summary.starts_with("Tool completed"));
        assert!(row.summary.contains("Command started"));
    }

    #[test]
    fn result_without_success_or_failure_evidence_is_unknown() {
        let mut model = ViewModel::new(false);
        model.ingest(row(1, "command_result", "a", "one", "unknown"));
        assert!(model.records[0].summary.starts_with("Tool outcome unknown"));
        assert!(model.records[0].summary.contains("start unavailable"));
        assert!(!model.records[0].summary.contains("Tool completed"));
    }

    #[test]
    fn failed_result_without_start_keeps_failure_visible_before_missing_start() {
        let mut model = ViewModel::new(false);
        model.ingest(json!({"sequence":1,"kind":"command_result","phase_id":"a",
            "provider":{"session_id":"one","item_id":"failed"},
            "body":{"nested":{"item":{"exit_code":1}}}}));
        assert!(model.records[0].summary.starts_with("Tool failed"));
        assert!(model.records[0].summary.contains("start unavailable"));
    }

    #[test]
    fn lifecycle_completion_without_outcome_evidence_stays_unknown() {
        for matched in [false, true] {
            let mut model = ViewModel::new(false);
            if matched {
                model.ingest(row(1, "command_start", "a", "one", "id"));
            }
            model.ingest(json!({"sequence":2,"kind":"command_result","phase_id":"a",
                "provider":{"session_id":"one","item_id":"id"},
                "body":{"nested":{"item":{"status":"completed"}}}}));
            assert!(model.records[0].summary.starts_with("Tool outcome unknown"));
            assert!(!model.records[0].summary.contains("Tool completed"));
            assert_eq!(
                model.records[0].summary.contains("start unavailable"),
                !matched
            );
        }
    }

    #[test]
    fn repeated_results_keep_one_outcome_and_original_command_identity() {
        let mut model = ViewModel::new(false);
        model.ingest(json!({"sequence":1,"kind":"command_start","phase_id":"a",
            "provider":{"session_id":"one","item_id":"repeat"},
            "body":{"nested":{"item":{"command":"echo retained identity"}}}}));
        for sequence in 2..20 {
            model.ingest(
                json!({"sequence":sequence,"kind":"command_result","phase_id":"a",
                "provider":{"session_id":"one","item_id":"repeat"},
                "body":{"nested":{"item":{"exit_code":0}}}}),
            );
        }
        assert_eq!(model.records.len(), 1);
        assert_eq!(
            model.records[0].summary.matches("Tool completed").count(),
            1
        );
        assert!(model.records[0].summary.contains("echo retained identity"));
    }

    #[test]
    fn truncated_success_evidence_is_unknown_but_visible_failure_is_retained() {
        for (exit_code, prefix) in [(0, "Tool outcome unknown"), (1, "Tool failed")] {
            let mut model = ViewModel::new(false);
            model.ingest(json!({"sequence":1,"kind":"command_result","phase_id":"a",
                "complete":false,"provider":{"session_id":"one","item_id":"truncated"},
                "body":{"nested":{"item":{"exit_code":exit_code}}}}));
            assert!(model.records[0].summary.starts_with(prefix));
            assert!(!model.records[0].complete);
        }
    }

    #[test]
    fn null_item_id_does_not_hide_available_call_id() {
        let mut model = ViewModel::new(false);
        for (sequence, kind) in [(1, "tool_start"), (2, "tool_result")] {
            model.ingest(json!({"sequence":sequence,"kind":kind,"phase_id":"code",
                "provider":{"session_id":"session","item_id":null,"call_id":"call"},
                "body":{"nested":{"tool_use_id":"call"}}}));
        }
        assert_eq!(model.records.len(), 1);
        assert!(!model.records[0].unresolved);
    }

    #[test]
    fn search_filter_and_empty_selection_are_stable() {
        let mut model = ViewModel::new(false);
        model.move_selection(1);
        model.ingest(row(1, "command_start", "a", "one", "tool"));
        model.search = "absent".into();
        model.set_filter(Filter::Problems);
        assert!(model.selected().is_none());
        model.search.clear();
        model.set_filter(Filter::All);
        assert!(model.selected().is_some());
    }

    #[test]
    fn retention_and_large_detail_are_bounded() {
        let mut model = ViewModel::new(false);
        for sequence in 0..MAX_RECORDS as u64 {
            model.ingest(json!({"sequence":sequence,"kind":"assistant_message",
                "body":{"text":"small"}}));
        }
        model.ingest(json!({"sequence":MAX_RECORDS,"kind":"assistant_message",
            "body":{"text":"x".repeat(MAX_DETAIL_BYTES * 2)}}));
        assert_eq!(model.records.len(), MAX_RECORDS);
        assert!(
            model
                .records
                .iter()
                .all(|row| row.detail.len() < MAX_DETAIL_BYTES + 64)
        );
        assert!(model.evicted_records > 0);
    }
}
