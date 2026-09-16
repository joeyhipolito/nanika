use super::*;
use serde_json::json;

const SUCCESS: &str = include_str!("../tests/fixtures/claude-2.1.269-coding-success.jsonl");

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn records() -> TestResult<Vec<Value>> {
    SUCCESS
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn parse(records: &[Value]) -> Result<ParsedOutput, ClaudeOutputError> {
    let wire = records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    parse_coding_output(wire.as_bytes(), Path::new("/workspace"), "sonnet", 5)
}

fn stream_pair(input: Value) -> TestResult<[Value; 2]> {
    let session = records()?[0]["session_id"].clone();
    Ok([
        json!({"type":"stream_event","session_id":session,"event":{"type":"content_block_start","index":99,"content_block":{"type":"tool_use","id":"orphan","name":"Write","input":input,"caller":{"type":"direct"}}}}),
        json!({"type":"stream_event","session_id":session,"event":{"type":"content_block_stop","index":99}}),
    ])
}

#[test]
fn streamed_outside_and_protected_paths_are_rejected_before_normalization() -> TestResult {
    for path in ["/outside/file", "/workspace/.claude/settings.json"] {
        let mut wire = records()?;
        wire.splice(1..1, stream_pair(json!({"file_path":path,"content":"x"}))?);
        assert!(matches!(parse(&wire), Err(ClaudeOutputError::ToolPath)));
    }
    Ok(())
}

#[test]
fn closed_streamed_orphan_cannot_hide_behind_an_unrelated_successful_edit() -> TestResult {
    let mut wire = records()?;
    wire.splice(
        1..1,
        stream_pair(json!({"file_path":"/workspace/orphan.txt","content":"x"}))?,
    );
    assert!(matches!(
        parse(&wire),
        Err(ClaudeOutputError::MissingResult)
    ));
    Ok(())
}

#[test]
fn streamed_argument_drift_malformed_json_and_unknown_delta_fields_are_rejected() -> TestResult {
    for case in 0..5 {
        let mut wire = records()?;
        let delta = wire
            .iter_mut()
            .find_map(|row| {
                let delta = row.pointer_mut("/event/delta")?;
                (delta["type"] == "input_json_delta").then_some(delta)
            })
            .ok_or("streamed tool arguments")?;
        match case {
            0 => {
                delta["unreviewed"] = json!(true);
            }
            1 => {
                delta["partial_json"] = json!("{");
            }
            2 => {
                delta["partial_json"] =
                    json!(r#"{"file_path":"/workspace/a","file_path":"/workspace/b"}"#);
            }
            3 | 4 => {
                let mut input: Value =
                    serde_json::from_str(delta["partial_json"].as_str().ok_or("argument JSON")?)?;
                input["file_path"] = json!(if case == 3 {
                    "/workspace/other.txt"
                } else {
                    "/outside/file"
                });
                delta["partial_json"] = json!(input.to_string());
            }
            _ => unreachable!(),
        }
        assert!(parse(&wire).is_err(), "accepted stream corruption {case}");
    }
    Ok(())
}

#[test]
fn streamed_id_must_match_an_authoritative_call() -> TestResult {
    let mut wire = records()?;
    let block = wire
        .iter_mut()
        .find_map(|row| {
            let block = row.pointer_mut("/event/content_block")?;
            (block["type"] == "tool_use").then_some(block)
        })
        .ok_or("streamed tool start")?;
    block["id"] = json!("unmatched-id");
    assert!(matches!(
        parse(&wire),
        Err(ClaudeOutputError::MissingResult)
    ));
    Ok(())
}

#[test]
fn authoritative_tool_call_may_arrive_before_its_stream_closes() -> TestResult {
    let mut wire = records()?;
    let index = wire
        .iter()
        .position(|row| {
            row.pointer("/message/content/0/name")
                .is_some_and(|name| name == "Edit")
        })
        .ok_or("authoritative edit")?;
    let call = wire.remove(index);
    let close = wire
        .iter()
        .position(|row| {
            row.pointer("/event/type")
                .is_some_and(|kind| kind == "content_block_stop")
        })
        .ok_or("stream close")?;
    wire.insert(close, call);
    assert!(parse(&wire).is_ok());
    Ok(())
}

#[test]
fn untyped_or_error_payloads_cannot_establish_a_successful_mutation() -> TestResult {
    for payload in [
        json!(false),
        json!(42),
        json!({}),
        json!({"error":"failed"}),
        json!("not an observed result"),
    ] {
        let mut wire = records()?;
        let row = wire
            .iter_mut()
            .find(|row| row.pointer("/tool_use_result/oldString").is_some())
            .ok_or("edit result")?;
        row["tool_use_result"] = payload;
        assert!(parse(&wire).is_err());
    }
    Ok(())
}

#[test]
fn result_paths_must_be_inside_workspace_and_match_the_call() -> TestResult {
    for path in ["/outside/file", "/workspace/other.txt"] {
        let mut wire = records()?;
        let row = wire
            .iter_mut()
            .find(|row| row.pointer("/tool_use_result/oldString").is_some())
            .ok_or("edit result")?;
        row["tool_use_result"]["filePath"] = json!(path);
        assert!(matches!(parse(&wire), Err(ClaudeOutputError::ToolPath)));
    }
    Ok(())
}

#[test]
fn one_payload_cannot_account_for_multiple_result_ids() -> TestResult {
    let mut wire = records()?;
    let row = wire
        .iter_mut()
        .find(|row| row.pointer("/tool_use_result/oldString").is_some())
        .ok_or("edit result")?;
    let duplicate = row["message"]["content"][0].clone();
    row["message"]["content"]
        .as_array_mut()
        .ok_or("results")?
        .push(duplicate);
    assert!(matches!(parse(&wire), Err(ClaudeOutputError::Malformed)));
    Ok(())
}

#[test]
fn subagent_parent_is_rejected_on_assistant_and_stream_records() -> TestResult {
    for kind in ["assistant", "stream_event"] {
        let mut wire = records()?;
        let row = wire
            .iter_mut()
            .find(|row| row["type"] == kind)
            .ok_or("record")?;
        row["parent_tool_use_id"] = json!("subagent-parent");
        assert!(matches!(
            parse(&wire),
            Err(ClaudeOutputError::ForbiddenCodingActivity)
        ));
    }
    Ok(())
}

#[test]
fn omitted_edit_default_agrees_across_wire_stream_and_authoritative_inputs() -> TestResult {
    for close_first in [false, true] {
        let mut wire = records()?;
        for row in &mut wire {
            if let Some(input) = row.pointer_mut("/wire_tool_inputs/tool-edit") {
                input
                    .as_object_mut()
                    .ok_or("wire input")?
                    .remove("replace_all");
            }
            if let Some(delta) = row.pointer_mut("/event/delta") {
                if delta["type"] == "input_json_delta" {
                    let mut input: Value =
                        serde_json::from_str(delta["partial_json"].as_str().ok_or("JSON")?)?;
                    input
                        .as_object_mut()
                        .ok_or("stream input")?
                        .remove("replace_all");
                    delta["partial_json"] = json!(input.to_string());
                }
            }
        }
        if close_first {
            let stop = wire
                .iter()
                .position(|row| {
                    row.pointer("/event/type")
                        .is_some_and(|kind| kind == "content_block_stop")
                })
                .ok_or("stop")?;
            let row = wire.remove(stop);
            wire.insert(stop - 1, row);
        }
        assert!(parse(&wire).is_ok());
    }
    Ok(())
}

#[test]
fn edit_default_equivalence_rejects_changed_values_and_unknown_fields() -> TestResult {
    for location in ["/wire_tool_inputs/tool-edit", "/event/delta"] {
        for value in [json!(true), Value::Null, json!("false")] {
            let mut wire = records()?;
            let target = wire
                .iter_mut()
                .find_map(|row| row.pointer_mut(location))
                .ok_or("input")?;
            if location == "/event/delta" {
                let mut input: Value =
                    serde_json::from_str(target["partial_json"].as_str().ok_or("JSON")?)?;
                input["replace_all"] = value;
                target["partial_json"] = json!(input.to_string());
            } else {
                target["replace_all"] = value;
            }
            assert!(parse(&wire).is_err());
        }
    }
    let mut wire = records()?;
    let input = wire
        .iter_mut()
        .find_map(|row| row.pointer_mut("/wire_tool_inputs/tool-edit"))
        .ok_or("wire input")?;
    input.as_object_mut().ok_or("object")?.remove("replace_all");
    input["unreviewed"] = json!(false);
    assert!(parse(&wire).is_err());
    Ok(())
}
