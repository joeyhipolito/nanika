//! Observational post-attempt telemetry wrapper around `usage::report`.
//! Never execution acceptance or canonical accounting.
use crate::usage;
use serde_json::{Value, json};

const SCHEMA: &str = "nanika.worker-usage.v1";
const IMPLEMENTATION_REVISION: &str = "usage-runtime/v1";

fn envelope(runtime: &str, status: &str, extra: Value) -> Value {
    let mut value = json!({
        "schema": SCHEMA,
        "runtime": runtime,
        "implementation_revision": IMPLEMENTATION_REVISION,
        "portal_requested": "off",
        "portal_effective": "off",
        "mode_source": "runtime-default",
        "independent_of_execution_acceptance": true,
        "status": status,
    });
    if let (Value::Object(base), Value::Object(more)) = (&mut value, extra) {
        base.extend(more);
    }
    value
}

pub(crate) fn capture(runtime: &str, stdout: Option<&[u8]>, discarded_bytes: Option<u64>) -> Value {
    if runtime != "claude" {
        return envelope(runtime, "not_supported", json!({}));
    }
    let Some(stdout) = stdout else {
        return envelope(runtime, "unavailable", json!({}));
    };
    if discarded_bytes != Some(0) {
        return envelope(runtime, "incomplete", json!({"reason": "discarded_bytes"}));
    }
    let mut report = match usage::report(stdout) {
        Ok(report) => report,
        Err(_) => return envelope(runtime, "unavailable", json!({})),
    };
    if report["assistant_message_count"].as_u64() == Some(0) {
        return envelope(runtime, "unavailable", json!({}));
    }
    report["portal_mode"] = json!("off");
    let events: Vec<Value> = report["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|message| {
            json!({
                "kind": "worker.usage",
                "delivery": "attempt-final-snapshot",
                "session_id": message["session_id"],
                "message_id": message["message_id"],
                "message_ordinal": message["ordinal"],
                "usage": message["usage"],
                "ambiguous": message["ambiguous"],
                "observed_input_tokens": message["observed_input_tokens"],
                "own_tool_names": message["own_tool_names"],
                "final_stream_output_observed": message["final_stream_output_observed"],
            })
        })
        .collect();
    envelope(
        runtime,
        "available",
        json!({"report": report, "events": events}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_claude_runtime_is_not_supported() {
        let value = capture("codex", Some(b"{}"), None);
        assert_eq!(value["status"], "not_supported");
        assert_eq!(value["runtime"], "codex");
        assert!(value.get("report").is_none());
    }

    #[test]
    fn missing_source_is_unavailable() {
        let value = capture("claude", None, None);
        assert_eq!(value["status"], "unavailable");
        assert!(value.get("report").is_none());
    }

    #[test]
    fn discarded_bytes_are_incomplete_without_totals() {
        let value = capture("claude", Some(b"{}"), Some(3));
        assert_eq!(value["status"], "incomplete");
        assert_eq!(value["reason"], "discarded_bytes");
        assert!(value.get("report").is_none());

        let unknown = capture("claude", Some(b"{}"), None);
        assert_eq!(unknown["status"], "incomplete");
    }

    #[test]
    fn malformed_source_is_unavailable_with_safe_static_message() {
        let value = capture("claude", Some(b"not json"), Some(0));
        assert_eq!(value["status"], "unavailable");
        assert!(value.get("report").is_none());
    }

    #[test]
    fn dedup_message_snapshots_become_one_event_each() -> Result<(), String> {
        let row = json!({"type":"assistant","session_id":"s","message":{"id":"m","usage":{
            "input_tokens":2,"output_tokens":4,"cache_read_input_tokens":0,"cache_creation_input_tokens":0
        },"content":[]}});
        let wire = format!("{row}\n{row}");
        let value = capture("claude", Some(wire.as_bytes()), Some(0));
        assert_eq!(value["status"], "available");
        let events = value["events"].as_array().ok_or("events array")?;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "worker.usage");
        assert_eq!(events[0]["delivery"], "attempt-final-snapshot");
        assert_eq!(events[0]["session_id"], "s");
        assert_eq!(events[0]["message_id"], "m");
        Ok(())
    }

    #[test]
    fn null_missing_counters_stay_null_in_events() -> Result<(), String> {
        let row = json!({"type":"assistant","session_id":"s","message":{"id":"m","usage":{
            "input_tokens":2
        },"content":[]}});
        let value = capture("claude", Some(row.to_string().as_bytes()), Some(0));
        assert_eq!(value["status"], "available");
        let events = value["events"].as_array().ok_or("events array")?;
        assert!(events[0]["observed_input_tokens"].is_null());
        assert!(events[0]["usage"]["output_tokens"].is_null());
        Ok(())
    }

    #[test]
    fn provider_result_evidence_is_unchanged_and_no_raw_stdout_is_copied() -> Result<(), String> {
        let assistant = json!({"type":"assistant","session_id":"s","message":{"id":"m","usage":{
            "input_tokens":2,"output_tokens":4,"cache_read_input_tokens":0,"cache_creation_input_tokens":0
        },"content":[{"type":"text","text":"SECRET_RAW"}]}});
        let result = json!({"type":"result","session_id":"s","num_turns":3,"total_cost_usd":1.0,"subtype":"error_max_turns","is_error":true,"result":"SECRET_RAW"});
        let wire = format!("{assistant}\n{result}");
        let value = capture("claude", Some(wire.as_bytes()), Some(0));
        assert_eq!(value["status"], "available");
        assert_eq!(value["report"]["provider_results"]["s"]["num_turns"], 3);
        assert_eq!(value["report"]["provider_results"]["s"]["is_error"], true);
        assert!(!value.to_string().contains("SECRET_RAW"));
        Ok(())
    }
}
