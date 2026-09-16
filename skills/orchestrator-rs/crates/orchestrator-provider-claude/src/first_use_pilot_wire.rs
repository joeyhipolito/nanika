//! Claude Code 2.1.269 stream-json drift admitted only by the first-use pilot
//! contract (docs/rust-orchestrator/FIRST-USE.md, "Observed 2.1.269 drift").
//!
//! The strict 2.1.211 wire types stay unchanged. Each pilot line is first
//! checked for duplicate keys by the caller. This module then validates and
//! removes exactly the additions recorded from a real 2.1.269 run, and hands
//! the remaining JSON back to the unchanged strict parser. Any other field,
//! value, or record still fails closed there. Two new records carry no output,
//! `rate_limit_event` and `system/status`, and they are validated and dropped
//! here.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{ClaudeOutputError, IterationUsageWire, ModelUsageWire, UsageWire};

/// Built-in subagent definitions 2.1.269 lists in `system/init` even with
/// `--tools ""`. Any other agent means a settings/plugin source leaked in.
const BUILT_IN_AGENTS: [&str; 5] = [
    "claude",
    "claude-code-guide",
    "Explore",
    "general-purpose",
    "Plan",
];

/// `system/init` capabilities observed from 2.1.269.
const OBSERVED_CAPABILITIES: [&str; 3] = [
    "interrupt_receipt_v1",
    "interrupt_cancel_queued_v1",
    "msg_lifecycle_v1",
];

pub(crate) enum PilotLine {
    /// A validated record without output or authority.
    Drop,
    /// JSON for the strict parser, with the admitted drift removed.
    Strict(String),
}

type Object = Map<String, Value>;

pub(crate) fn normalize(json: &str) -> Result<PilotLine, ClaudeOutputError> {
    let mut value: Value = serde_json::from_str(json).map_err(|_| ClaudeOutputError::Malformed)?;
    {
        let record = as_object(&mut value)?;
        match record.get("type").and_then(Value::as_str) {
            Some("rate_limit_event") => {
                validate_rate_limit_event(record)?;
                return Ok(PilotLine::Drop);
            }
            Some("system") => match record.get("subtype").and_then(Value::as_str) {
                Some("status") => {
                    validate_status(record)?;
                    return Ok(PilotLine::Drop);
                }
                Some("init") => normalize_init(record)?,
                _ => {}
            },
            Some("stream_event") => normalize_stream_event(record)?,
            Some("assistant") => normalize_assistant(record)?,
            Some("result") => normalize_result(record)?,
            _ => {}
        }
    }
    serde_json::to_string(&value)
        .map(PilotLine::Strict)
        .map_err(|_| ClaudeOutputError::Malformed)
}

fn validate_rate_limit_event(record: &Object) -> Result<(), ClaudeOutputError> {
    only_keys(record, &["type", "rate_limit_info", "uuid", "session_id"])?;
    optional_strings(record, &["uuid", "session_id"])?;
    let info = record
        .get("rate_limit_info")
        .and_then(Value::as_object)
        .ok_or(ClaudeOutputError::Malformed)?;
    only_keys(
        info,
        &[
            "status",
            "resetsAt",
            "rateLimitType",
            "utilization",
            "isUsingOverage",
            "overageStatus",
            "overageDisabledReason",
            "unifiedWindows",
        ],
    )?;
    match info.get("status").and_then(Value::as_str) {
        Some("allowed" | "allowed_warning") => {}
        Some("rejected") => return Err(ClaudeOutputError::ProviderReportedError),
        Some(_) => return Err(ClaudeOutputError::UnknownProtocolValue),
        None => return Err(ClaudeOutputError::Malformed),
    }
    optional_exact_string(info, "overageStatus", "rejected")?;
    optional_exact_string(info, "overageDisabledReason", "out_of_credits")?;
    let typed = info.get("resetsAt").is_none_or(Value::is_u64)
        && info.get("rateLimitType").is_none_or(Value::is_string)
        && info.get("utilization").is_none_or(Value::is_number)
        && info.get("isUsingOverage").is_none_or(Value::is_boolean);
    if !typed {
        return Err(ClaudeOutputError::Malformed);
    }
    if let Some(windows) = info.get("unifiedWindows") {
        let windows = windows.as_object().ok_or(ClaudeOutputError::Malformed)?;
        only_keys(windows, &["five_hour", "seven_day"])?;
        for window in windows.values() {
            let window = window.as_object().ok_or(ClaudeOutputError::Malformed)?;
            only_keys(window, &["utilization", "resetsAt"])?;
            if !(window.get("utilization").is_none_or(Value::is_number)
                && window.get("resetsAt").is_none_or(Value::is_u64))
            {
                return Err(ClaudeOutputError::Malformed);
            }
        }
    }
    Ok(())
}

fn validate_status(record: &Object) -> Result<(), ClaudeOutputError> {
    only_keys(record, &["type", "subtype", "status", "uuid", "session_id"])?;
    optional_strings(record, &["uuid", "session_id"])?;
    match record.get("status").and_then(Value::as_str) {
        Some("requesting") => Ok(()),
        Some(_) => Err(ClaudeOutputError::UnknownProtocolValue),
        None => Err(ClaudeOutputError::Malformed),
    }
}

fn normalize_init(record: &mut Object) -> Result<(), ClaudeOutputError> {
    remove_string(record, "messaging_socket_path")?;
    remove_exact_string(record, "fast_mode_disabled_reason", "sdk_opt_in_required")?;
    empty_allowlisted(record, "agents", &BUILT_IN_AGENTS)?;
    empty_allowlisted(record, "capabilities", &OBSERVED_CAPABILITIES)
}

fn normalize_stream_event(record: &mut Object) -> Result<(), ClaudeOutputError> {
    remove_u64(record, "ttft_ms")?;
    let Some(event) = record.get_mut("event").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    match event.get("type").and_then(Value::as_str) {
        Some("message_start") => {
            if let Some(message) = event.get_mut("message") {
                remove_null(as_object(message)?, "container")?;
            }
        }
        Some("content_block_delta") => {
            let thinking_delta = event
                .get_mut("delta")
                .and_then(Value::as_object_mut)
                .filter(|delta| {
                    delta.get("type").and_then(Value::as_str) == Some("thinking_delta")
                });
            if let Some(delta) = thinking_delta {
                match delta.remove("estimated_tokens") {
                    None | Some(Value::Null) => {}
                    Some(tokens) if tokens.is_u64() => {}
                    Some(_) => return Err(ClaudeOutputError::Malformed),
                }
            }
        }
        Some("message_delta") => {
            if let Some(delta) = event.get_mut("delta") {
                let delta = as_object(delta)?;
                remove_null(delta, "stop_details")?;
                remove_null(delta, "container")?;
            }
            match event.remove("context_management") {
                None => {}
                Some(Value::Object(management))
                    if management.len() == 1
                        && management
                            .get("applied_edits")
                            .and_then(Value::as_array)
                            .is_some_and(Vec::is_empty) => {}
                Some(_) => return Err(ClaudeOutputError::UnknownProtocolValue),
            }
            if let Some(usage) = event.get_mut("usage") {
                let usage = as_object(usage)?;
                remove_output_tokens_details(usage)?;
                remove_iterations(usage)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn normalize_assistant(record: &mut Object) -> Result<(), ClaudeOutputError> {
    remove_string(record, "timestamp")?;
    if let Some(message) = record.get_mut("message") {
        remove_null(as_object(message)?, "container")?;
    }
    Ok(())
}

fn normalize_result(record: &mut Object) -> Result<(), ClaudeOutputError> {
    remove_exact_string(record, "fast_mode_disabled_reason", "sdk_opt_in_required")?;
    remove_u64(record, "first_content_frame_ms")?;
    remove_zero(record, "queued_turn_count")?;
    remove_zero(record, "result_index")?;
    if let Some(stats) = record.remove("subagent_stats") {
        validate_no_subagents(&stats)?;
    }
    if let Some(usage) = record.get_mut("usage") {
        remove_output_tokens_details(as_object(usage)?)?;
    }
    if let Some(model_usage) = record.remove("modelUsage") {
        validate_model_usage_with_auxiliary_models(model_usage, record)?;
    }
    Ok(())
}

/// 2.1.269 lists auxiliary models (observed: a Haiku call) in `modelUsage`.
/// Their cost is part of `total_cost_usd`, but their tokens are not part of
/// `usage`. The strict 2.1.211 check sums every entry against `usage`, so the
/// pilot validates the map here and strips it before strict parsing. Exactly
/// one entry must match `usage` token for token. Every entry must be
/// well-formed with zero web searches, and all costs must sum to
/// `total_cost_usd`.
fn validate_model_usage_with_auxiliary_models(
    model_usage: Value,
    record: &Object,
) -> Result<(), ClaudeOutputError> {
    let Value::Object(mut entries) = model_usage else {
        return Err(ClaudeOutputError::Malformed);
    };
    for entry in entries.values_mut() {
        let entry = as_object(entry)?;
        remove_u64(entry, "thinkingTokens")?;
        remove_string(entry, "canonicalModel")?;
        remove_exact_string(entry, "provider", "firstParty")?;
        remove_exact_string(entry, "costBasis", "list")?;
    }
    let entries: BTreeMap<String, ModelUsageWire> =
        serde_json::from_value(Value::Object(entries)).map_err(|_| ClaudeOutputError::Malformed)?;
    let usage: UsageWire = record
        .get("usage")
        .cloned()
        .ok_or(ClaudeOutputError::InvalidCost)
        .and_then(|usage| {
            serde_json::from_value(usage).map_err(|_| ClaudeOutputError::Malformed)
        })?;
    let total_cost_usd = record
        .get("total_cost_usd")
        .and_then(Value::as_f64)
        .ok_or(ClaudeOutputError::InvalidCost)?;
    let mut primary = 0_usize;
    let mut cost_usd = 0.0;
    for (model, entry) in &entries {
        if entry.web_search_requests != 0 {
            return Err(ClaudeOutputError::ToolProtocol);
        }
        if model.is_empty()
            || entry.context_window == 0
            || entry.max_output_tokens == 0
            || !entry.cost_usd.is_finite()
            || entry.cost_usd < 0.0
        {
            return Err(ClaudeOutputError::InvalidCost);
        }
        if entry.input_tokens == usage.input_tokens
            && entry.output_tokens == usage.output_tokens
            && entry.cache_read_input_tokens == usage.cache_read_input_tokens
            && entry.cache_creation_input_tokens == usage.cache_creation_input_tokens
        {
            primary += 1;
        }
        cost_usd += entry.cost_usd;
    }
    let cost_tolerance = f64::EPSILON * 16.0 * total_cost_usd.abs().max(1.0);
    if primary != 1 || !cost_usd.is_finite() || (cost_usd - total_cost_usd).abs() > cost_tolerance {
        return Err(ClaudeOutputError::InvalidCost);
    }
    Ok(())
}

/// Every subagent counter must be zero: a tool-less review spawns none.
fn validate_no_subagents(stats: &Value) -> Result<(), ClaudeOutputError> {
    const COUNTERS: [&str; 6] = [
        "spawned",
        "started_in_background",
        "max_depth",
        "spawned_by_subagents",
        "completed",
        "failed",
    ];
    const GROUPS: [(&str, &[&str]); 3] = [
        ("requested", &["background", "foreground", "unset"]),
        ("killed", &["parent", "user", "system"]),
        ("refused", &["depth_limit", "concurrency_limit", "budget"]),
    ];
    let stats = stats.as_object().ok_or(ClaudeOutputError::Malformed)?;
    let mut allowed: Vec<&str> = COUNTERS.to_vec();
    allowed.extend(GROUPS.iter().map(|(group, _)| *group));
    allowed.push("by_type");
    only_keys(stats, &allowed)?;
    zero_counters(stats, &COUNTERS)?;
    for (group, counters) in GROUPS {
        let group = stats
            .get(group)
            .and_then(Value::as_object)
            .ok_or(ClaudeOutputError::Malformed)?;
        only_keys(group, counters)?;
        zero_counters(group, counters)?;
    }
    match stats.get("by_type").and_then(Value::as_object) {
        Some(by_type) if by_type.is_empty() => Ok(()),
        Some(_) => Err(ClaudeOutputError::ToolProtocol),
        None => Err(ClaudeOutputError::Malformed),
    }
}

fn zero_counters(object: &Object, counters: &[&str]) -> Result<(), ClaudeOutputError> {
    for counter in counters {
        match object.get(*counter).and_then(Value::as_u64) {
            Some(0) => {}
            Some(_) => return Err(ClaudeOutputError::ToolProtocol),
            None => return Err(ClaudeOutputError::Malformed),
        }
    }
    Ok(())
}

/// `message_delta.usage.iterations` is new in 2.1.269; the strict 2.1.211
/// stream usage type has no such field. Each entry is validated with the
/// strict result-usage iteration type before removal.
fn remove_iterations(usage: &mut Object) -> Result<(), ClaudeOutputError> {
    let Some(iterations) = usage.remove("iterations") else {
        return Ok(());
    };
    let iterations: Vec<IterationUsageWire> =
        serde_json::from_value(iterations).map_err(|_| ClaudeOutputError::Malformed)?;
    iterations.iter().try_for_each(IterationUsageWire::validate)
}

fn remove_output_tokens_details(usage: &mut Object) -> Result<(), ClaudeOutputError> {
    match usage.remove("output_tokens_details") {
        None => Ok(()),
        Some(Value::Object(details))
            if details.keys().all(|key| key == "thinking_tokens")
                && details.values().all(Value::is_u64) =>
        {
            Ok(())
        }
        Some(_) => Err(ClaudeOutputError::Malformed),
    }
}

fn empty_allowlisted(
    record: &mut Object,
    key: &str,
    allowed: &[&str],
) -> Result<(), ClaudeOutputError> {
    let Some(values) = record.get_mut(key) else {
        return Ok(());
    };
    let items = values.as_array().ok_or(ClaudeOutputError::Malformed)?;
    for item in items {
        let item = item.as_str().ok_or(ClaudeOutputError::Malformed)?;
        if !allowed.contains(&item) {
            return Err(ClaudeOutputError::UnisolatedRuntime);
        }
    }
    *values = Value::Array(Vec::new());
    Ok(())
}

fn as_object(value: &mut Value) -> Result<&mut Object, ClaudeOutputError> {
    value.as_object_mut().ok_or(ClaudeOutputError::Malformed)
}

fn only_keys(object: &Object, allowed: &[&str]) -> Result<(), ClaudeOutputError> {
    if object.keys().all(|key| allowed.contains(&key.as_str())) {
        Ok(())
    } else {
        Err(ClaudeOutputError::Malformed)
    }
}

fn optional_strings(object: &Object, keys: &[&str]) -> Result<(), ClaudeOutputError> {
    if keys
        .iter()
        .all(|key| object.get(*key).is_none_or(Value::is_string))
    {
        Ok(())
    } else {
        Err(ClaudeOutputError::Malformed)
    }
}

fn optional_exact_string(
    object: &Object,
    key: &str,
    expected: &str,
) -> Result<(), ClaudeOutputError> {
    match object.get(key) {
        None => Ok(()),
        Some(Value::String(value)) if value == expected => Ok(()),
        Some(Value::String(_)) => Err(ClaudeOutputError::UnknownProtocolValue),
        Some(_) => Err(ClaudeOutputError::Malformed),
    }
}

fn remove_null(object: &mut Object, key: &str) -> Result<(), ClaudeOutputError> {
    match object.remove(key) {
        None | Some(Value::Null) => Ok(()),
        Some(_) => Err(ClaudeOutputError::UnknownProtocolValue),
    }
}

fn remove_string(object: &mut Object, key: &str) -> Result<(), ClaudeOutputError> {
    match object.remove(key) {
        None | Some(Value::String(_)) => Ok(()),
        Some(_) => Err(ClaudeOutputError::Malformed),
    }
}

fn remove_exact_string(
    object: &mut Object,
    key: &str,
    expected: &str,
) -> Result<(), ClaudeOutputError> {
    match object.remove(key) {
        None => Ok(()),
        Some(Value::String(value)) if value == expected => Ok(()),
        Some(Value::String(_)) => Err(ClaudeOutputError::UnknownProtocolValue),
        Some(_) => Err(ClaudeOutputError::Malformed),
    }
}

fn remove_u64(object: &mut Object, key: &str) -> Result<(), ClaudeOutputError> {
    match object.remove(key) {
        None => Ok(()),
        Some(value) if value.is_u64() => Ok(()),
        Some(_) => Err(ClaudeOutputError::Malformed),
    }
}

fn remove_zero(object: &mut Object, key: &str) -> Result<(), ClaudeOutputError> {
    match object.remove(key).map(|value| value.as_u64()) {
        None | Some(Some(0)) => Ok(()),
        Some(Some(_)) => Err(ClaudeOutputError::UnknownProtocolValue),
        Some(None) => Err(ClaudeOutputError::Malformed),
    }
}
