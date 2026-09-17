use crate::{
    usage_codex::{CodexUsage, report},
    usage_runtime,
};
use serde_json::{Value, json};

fn stream(rows: &[Value]) -> Vec<u8> {
    rows.iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}
fn rows(usage: Value) -> Vec<Value> {
    vec![
        json!({"type":"thread.started","thread_id":"t"}),
        json!({"type":"turn.started"}),
        json!({"type":"turn.completed","usage":usage}),
    ]
}
fn full() -> Value {
    json!({"input_tokens":100,"cached_input_tokens":40,"cache_write_input_tokens":2,"output_tokens":10,"reasoning_output_tokens":3})
}

#[test]
fn recorded_codex_capture_is_turn_scoped_without_double_counting() -> Result<(), String> {
    let r = report(include_bytes!("codex-success.jsonl")).map_err(str::to_owned)?;
    assert_eq!(r["provider_turn_count"], 1);
    assert!(r["assistant_message_count"].is_null());
    assert_eq!(r["summary"]["input_tokens"], 10430);
    assert_eq!(r["summary"]["cache_read_input_tokens"], 6784);
    assert!(r["summary"]["cost_usd"].is_null());
    let turn = &r["turns"][0];
    assert_eq!(turn["granularity"], "provider-turn");
    assert_eq!(turn["turn_id"], "turn-1");
    assert_eq!(turn["uncached_input_tokens"], 3646);
    assert!(turn["message_id"].is_null());
    Ok(())
}
#[test]
fn duplicate_terminal_is_deduplicated_but_identical_next_turn_is_distinct() -> Result<(), String> {
    let mut wire = rows(full());
    wire.push(wire[2].clone());
    wire.push(json!({"type":"turn.started"}));
    wire.push(wire[2].clone());
    let r = report(&stream(&wire)).map_err(str::to_owned)?;
    assert_eq!(r["provider_turn_count"], 2);
    assert_eq!(r["quality"]["duplicate_terminal_count"], 1);
    assert_eq!(r["summary"]["input_tokens"], 200);
    assert_eq!(r["turns"][1]["turn_id"], "turn-2");
    Ok(())
}
#[test]
fn omitted_counters_are_null_and_no_raw_provider_text_is_copied() -> Result<(), String> {
    let mut wire = rows(json!({"input_tokens":100,"output_tokens":10}));
    wire.insert(2,json!({"type":"item.completed","item":{"id":"a","text":"PRIVATE_SENTINEL","arguments":{"password":"PRIVATE_SENTINEL"}}}));
    let r = report(&stream(&wire)).map_err(str::to_owned)?;
    assert!(r["summary"]["cache_creation_input_tokens"].is_null());
    assert!(r["summary"]["cache_read_input_tokens"].is_null());
    assert!(r["turns"][0]["uncached_input_tokens"].is_null());
    assert!(!r.to_string().contains("PRIVATE_SENTINEL"));
    Ok(())
}
#[test]
fn invalid_counter_shape_subset_and_duplicate_keys_are_refused() {
    for usage in [
        json!({"input_tokens":-1}),
        json!({"input_tokens":"1"}),
        json!({"input_tokens":null}),
        json!({"input_tokens":1,"cached_input_tokens":2}),
        json!({"output_tokens":1,"reasoning_output_tokens":2}),
    ] {
        assert!(report(&stream(&rows(usage))).is_err());
    }
    assert!(report(br#"{"type":"thread.started","thread_id":"t","thread_id":"other"}"#).is_err());
}
#[test]
fn invalid_ordering_poisons_collector() {
    let mut c = CodexUsage::default();
    assert!(
        c.observe(&json!({"type":"turn.completed","usage":full()}))
            .is_err()
    );
    assert!(c.finish().is_err());
    let mut wire = rows(full());
    wire.push(json!({"type":"turn.completed","usage":{"input_tokens":999}}));
    assert!(report(&stream(&wire)).is_err());
    let mut wire = rows(full());
    wire.insert(2, json!({"type":"turn.started"}));
    assert!(report(&stream(&wire)).is_err());
}
#[test]
fn checked_totals_refuse_overflow() {
    let mut wire = rows(json!({"input_tokens":u64::MAX}));
    wire.push(json!({"type":"turn.started"}));
    wire.push(json!({"type":"turn.completed","usage":{"input_tokens":1}}));
    assert!(report(&stream(&wire)).is_err());
}
#[test]
fn incomplete_and_failed_turns_are_visible_without_inventing_usage() -> Result<(), String> {
    let mut wire = rows(full());
    wire.push(json!({"type":"turn.started"}));
    let r = report(&stream(&wire)).map_err(str::to_owned)?;
    assert_eq!(r["quality"]["incomplete_turn_count"], 1);
    assert_eq!(r["provider_turn_count"], 1);
    wire.push(json!({"type":"turn.failed","error":{"message":"PRIVATE_SENTINEL"}}));
    let r = report(&stream(&wire)).map_err(str::to_owned)?;
    assert_eq!(r["quality"]["failed_turn_count"], 1);
    assert_eq!(r["quality"]["incomplete_turn_count"], 0);
    assert!(!r.to_string().contains("PRIVATE_SENTINEL"));
    Ok(())
}
#[test]
fn runtime_capture_supports_codex_without_affecting_acceptance() {
    let wire = stream(&rows(full()));
    let r = usage_runtime::capture("codex", Some(&wire), Some(0));
    assert_eq!(r["status"], "available");
    assert_eq!(r["events"][0]["granularity"], "provider-turn");
    assert_eq!(r["events"][0]["delivery"], "attempt-final-snapshot");
    assert_eq!(
        usage_runtime::capture("codex", Some(&wire), Some(1))["status"],
        "incomplete"
    );
    assert_eq!(
        usage_runtime::capture("codex", None, None)["status"],
        "unavailable"
    );
}

#[test]
fn failed_turn_does_not_reuse_an_earlier_terminal_identity() {
    let mut wire = rows(full());
    wire.push(json!({"type":"turn.started"}));
    wire.push(json!({"type":"turn.failed"}));
    wire.push(wire[2].clone());
    assert!(report(&stream(&wire)).is_err());
}

#[test]
fn missing_counter_in_any_turn_makes_total_unknown() -> Result<(), String> {
    let mut wire = rows(full());
    wire.push(json!({"type":"turn.started"}));
    wire.push(json!({"type":"turn.completed","usage":{"output_tokens":4}}));
    let r = report(&stream(&wire)).map_err(str::to_owned)?;
    assert!(r["summary"]["input_tokens"].is_null());
    assert_eq!(r["summary"]["output_tokens"], 14);
    Ok(())
}
