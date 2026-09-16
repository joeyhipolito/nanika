//! Display-only provider vocabulary, bounded previews and redaction.
use super::{MAX_SNIPPET_BYTES, identifier};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
pub(super) struct ClaudeBlock {
    pub(super) id: String,
    input: String,
    omitted: usize,
}

pub(super) fn provider_observations(
    value: &Value,
    blocks: &mut BTreeMap<u64, ClaudeBlock>,
) -> Vec<(String, Value)> {
    let kind = value.get("type").and_then(Value::as_str);
    if matches!(kind, Some("assistant" | "user")) {
        if let Some(content) = value
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        {
            let mut observations = Vec::new();
            for block in content.iter().take(128) {
                let kind = match block.get("type").and_then(Value::as_str) {
                    Some("text") if kind == Some("user") => "user_message",
                    Some("text") => "assistant_message",
                    Some("tool_use") => "tool_start",
                    Some("tool_result") => "tool_result",
                    _ => "unknown",
                };
                observations.push((kind.to_owned(), block.clone()));
            }
            if content.len() > 128 {
                observations.push((
                    "truncated".into(),
                    json!({"reason":"message block limit","omitted_blocks":content.len()-128}),
                ));
            }
            return observations;
        }
    }
    if kind == Some("stream_event") {
        if let Some(event) = value.get("event") {
            let index = event.get("index").and_then(Value::as_u64);
            match event.get("type").and_then(Value::as_str) {
                Some("message_start") => {
                    blocks.clear();
                }
                Some("content_block_start") => {
                    if let Some(block) = event.get("content_block") {
                        if block["type"] == "tool_use" {
                            if let (Some(index), Some(id)) = (index, identifier(block, "id")) {
                                if blocks.len() >= 16 && !blocks.contains_key(&index) {
                                    return vec![
                                        ("tool_start".into(), block.clone()),
                                        (
                                            "lost".into(),
                                            json!({"reason":"open tool block limit","tool_use_id":id,"index":index}),
                                        ),
                                    ];
                                }
                                {
                                    blocks.insert(
                                        index,
                                        ClaudeBlock {
                                            id,
                                            input: String::new(),
                                            omitted: 0,
                                        },
                                    );
                                }
                            }
                            return vec![("tool_start".into(), block.clone())];
                        }
                    }
                }
                Some("content_block_delta") => {
                    let Some(delta) = event.get("delta") else {
                        return vec![("unknown".into(), bounded_unknown("missing-delta", event))];
                    };
                    if delta["type"] == "input_json_delta" {
                        let text = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let block = index.and_then(|i| blocks.get_mut(&i));
                        let id = block.as_ref().map(|b| b.id.clone());
                        if let Some(block) = block {
                            if block.omitted != 0
                                || block.input.len().saturating_add(text.len()) > MAX_SNIPPET_BYTES
                            {
                                block.omitted = block
                                    .omitted
                                    .saturating_add(block.input.len())
                                    .saturating_add(text.len());
                                block.input.clear();
                            } else {
                                block.input.push_str(text);
                            }
                        }
                        return vec![(
                            "tool_input_delta".into(),
                            json!({"tool_use_id":id,"bytes":text.len(),"preview":"withheld until a complete redacted argument object"}),
                        )];
                    }
                    return vec![("assistant_delta".into(), event.clone())];
                }
                Some("content_block_stop") => {
                    let block = index.and_then(|index| blocks.remove(&index));
                    let mut observations = Vec::new();
                    if let Some(block) = &block {
                        if block.omitted != 0 {
                            observations.push(("truncated".into(),json!({"tool_use_id":block.id,"reason":"tool argument byte limit","bytes":block.omitted})));
                        } else if !block.input.is_empty() {
                            match serde_json::from_str::<Value>(&block.input) {
                                Ok(arguments) => observations.push(("tool_input".into(),json!({"tool_use_id":block.id,"arguments":redact(arguments)}))),
                                Err(_) => observations.push(("incomplete".into(),json!({"tool_use_id":block.id,"reason":"invalid tool argument JSON; preview withheld"}))),
                            }
                        }
                    }
                    observations.push((
                        "tool_block_closed".into(),
                        json!({"tool_use_id":block.map(|b|b.id),"index":index}),
                    ));
                    return observations;
                }
                _ => {}
            }
        }
    }
    vec![normalize(value)]
}

pub(super) fn normalize(value: &Value) -> (String, Value) {
    let object = value.as_object();
    let kind = string(object, "kind");
    if string(object, "schema") == Some("nanika.rust-pilot.progress.v1") {
        return match kind {
            Some("journal") => ("owner_journal".into(), value.clone()),
            Some("phase_route") => ("owner_route".into(), value.clone()),
            Some("output_dropped" | "progress_dropped") => ("lost".into(), value.clone()),
            Some("heartbeat") => ("heartbeat".into(), value.clone()),
            Some("worker.usage") => ("usage".into(), value.clone()),
            Some("worker.usage_unavailable") => ("lost".into(), value.clone()),
            Some("process_output") => ("provider_chunk".into(), value.clone()),
            Some("stage") => ("owner_stage".into(), value.clone()),
            Some(other) => ("unknown".into(), bounded_unknown(other, value)),
            None => (
                "malformed".into(),
                json!({"reason":"progress wrapper has no kind"}),
            ),
        };
    }
    let event_type = string(object, "type");
    match event_type {
        Some("thread.started") => ("provider_session".into(), value.clone()),
        Some("turn.started") => ("provider_turn".into(), value.clone()),
        Some("turn.completed") => ("usage".into(), value.clone()),
        Some("item.started") => codex_item(value, "start"),
        Some("item.updated") => codex_item(value, "update"),
        Some("item.completed") => codex_item(value, "result"),
        Some("assistant") => ("assistant_message".into(), value.clone()),
        Some("result") => ("provider_result".into(), value.clone()),
        Some("stream_event") => claude_stream(value),
        Some("error" | "turn.failed") => ("provider_error".into(), value.clone()),
        Some(other) => ("unknown".into(), bounded_unknown(other, value)),
        None => ("unknown".into(), bounded_unknown("untyped", value)),
    }
}

pub(super) fn is_process_output(value: &Value) -> bool {
    value.get("schema").and_then(Value::as_str) == Some("nanika.rust-pilot.progress.v1")
        && value.get("kind").and_then(Value::as_str) == Some("process_output")
}

fn codex_item(value: &Value, action: &str) -> (String, Value) {
    let item = value.get("item").and_then(Value::as_object);
    let item_type = string(item, "type");
    let kind = match (item_type, action) {
        (Some("agent_message"), _) => "assistant_message",
        (Some("command_execution"), "start") => "command_start",
        (Some("command_execution"), "update") => "command_update",
        (Some("command_execution"), _) => "command_result",
        (Some("file_change"), _) => "file_change",
        (Some("mcp_tool_call"), "start") => "mcp_tool_start",
        (Some("mcp_tool_call"), "update") => "mcp_tool_update",
        (Some("mcp_tool_call"), _) => "mcp_tool_result",
        (Some("error"), _) => "provider_error",
        _ => "unknown",
    };
    (
        kind.into(),
        if kind == "unknown" {
            bounded_unknown(item_type.unwrap_or("item"), value)
        } else {
            value.clone()
        },
    )
}

fn claude_stream(value: &Value) -> (String, Value) {
    let event = value.get("event").and_then(Value::as_object);
    let event_type = string(event, "type");
    let kind = match event_type {
        Some("content_block_start") => match event
            .and_then(|event| event.get("content_block"))
            .and_then(Value::as_object)
            .and_then(|block| string(Some(block), "type"))
        {
            Some("tool_use") => "tool_start",
            _ => "assistant_delta",
        },
        Some("content_block_delta") => "assistant_delta",
        Some("content_block_stop") => "tool_block_closed",
        Some("message_delta") => "usage",
        Some("error") => "provider_error",
        _ => "unknown",
    };
    (
        kind.into(),
        if kind == "unknown" {
            bounded_unknown(event_type.unwrap_or("stream_event"), value)
        } else {
            value.clone()
        },
    )
}

fn string<'a>(object: Option<&'a Map<String, Value>>, key: &str) -> Option<&'a str> {
    object.and_then(|map| map.get(key)).and_then(Value::as_str)
}

pub(super) fn bounded_unknown(event_type: &str, value: &Value) -> Value {
    let serialized = serde_json::to_string(value).unwrap_or_else(|_| String::new());
    json!({"event_type":event_type, "snippet":safe_text(&serialized), "truncated":serialized.len()>MAX_SNIPPET_BYTES})
}

// Only the outer transport text bypasses redaction until provider framing. Nested
// objects that happen to resemble transport wrappers receive ordinary redaction.
pub(super) fn redact_progress_chunk(mut value: Value) -> Value {
    let raw = value.as_object_mut().and_then(|map| map.remove("text"));
    let text_sensitive = std::env::var("NANIKA_OBSERVE_REDACT_FIELDS")
        .unwrap_or_default()
        .split(',')
        .any(|name| name.trim().eq_ignore_ascii_case("text"));
    let mut value = redact(value);
    if let (Some(map), Some(text)) = (value.as_object_mut(), raw) {
        map.insert(
            "text".into(),
            if text_sensitive { Value::Null } else { text },
        );
    }
    value
}

pub(super) fn redact(value: Value) -> Value {
    let configured = std::env::var("NANIKA_OBSERVE_REDACT_FIELDS").unwrap_or_default();
    let names: BTreeSet<String> = [
        "api_key",
        "apikey",
        "authorization",
        "password",
        "secret",
        "token",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(
        configured
            .split(',')
            .map(|name| name.trim().to_ascii_lowercase()),
    )
    .collect();
    redact_value(value, &names)
}

fn redact_value(value: Value, names: &BTreeSet<String>) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let redacted = if names.contains(&key.to_ascii_lowercase()) {
                        Value::String("[REDACTED]".into())
                    } else {
                        redact_value(value, names)
                    };
                    (key, redacted)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| redact_value(value, names))
                .collect(),
        ),
        Value::String(text)
            if text.trim_start().starts_with('{') || text.trim_start().starts_with('[') =>
        {
            match serde_json::from_str::<Value>(&text) {
                Ok(nested) => {
                    let start = text.len() - text.trim_start().len();
                    let end = text.trim_end().len();
                    Value::String(format!(
                        "{}{}{}",
                        &text[..start],
                        redact_value(nested, names),
                        &text[end..]
                    ))
                }
                Err(_) => Value::String(text),
            }
        }
        other => other,
    }
}

pub(crate) fn safe_text(text: &str) -> String {
    let mut result = String::new();
    for character in text.chars() {
        if result.len() >= MAX_SNIPPET_BYTES {
            result.push_str("…[truncated]");
            break;
        }
        if character.is_control() {
            result.push_str(&format!("\\u{{{:04x}}}", character as u32));
        } else {
            result.push(character);
        }
    }
    result
}
