use orchestrator_core::{
    CheckpointPhase, CheckpointPlan, CheckpointProjection, CheckpointSourceShape, EventJsonMap,
    EventRecord, EventScanDiagnosticKind, GO_EVENT_JSON_CONTENT_MAX_BYTES, GO_ZERO_TIME,
    GoJsonScalar, STABLE_EVENT_TYPES, decode_checkpoint, decode_event_line,
    decode_go_json_array_elements, decode_go_json_object_members, decode_go_json_scalar,
    decode_go_observed_event_line, decode_go_observed_event_record, encode_current_checkpoint,
    encode_current_event, encode_preserved_checkpoint, encode_preserved_event, scan_event_log,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const LEGACY: &[u8] =
    include_bytes!("../../../tests/fixtures/core-parity/checkpoints/legacy-direct-v1.json");
const DIRECT_V2: &[u8] =
    include_bytes!("../../../tests/fixtures/core-parity/checkpoints/legacy-direct-v2.json");
const FORWARD: &[u8] = include_bytes!(
    "../../../tests/fixtures/core-parity/checkpoints/envelope-v1-payload-v2-forward.json"
);
const EVENTS: &[u8] =
    include_bytes!("../../../tests/fixtures/core-parity/events/mixed-history.jsonl");

fn nested_data_array_event(array_depth: usize) -> Vec<u8> {
    let mut line = Vec::with_capacity(22 + array_depth * 2);
    line.extend_from_slice(br#"{"data":{"nested":"#);
    line.extend(std::iter::repeat_n(b'[', array_depth));
    line.push(b'0');
    line.extend(std::iter::repeat_n(b']', array_depth));
    line.extend_from_slice(b"}}");
    line
}

fn nested_ignored_array_event(array_depth: usize) -> Vec<u8> {
    let mut line = Vec::with_capacity(26 + array_depth * 2);
    line.extend_from_slice(br#"{"sequence":7,"future":"#);
    line.extend(std::iter::repeat_n(b'[', array_depth));
    line.push(b'0');
    line.extend(std::iter::repeat_n(b']', array_depth));
    line.push(b'}');
    line
}

fn singleton_array_leaf(mut value: &Value, depth: usize) -> Option<&Value> {
    for _ in 0..depth {
        value = value.as_array()?.first()?;
    }
    Some(value)
}

fn nested_array_value(depth: usize) -> Value {
    let mut value = Value::from(0.0);
    for _ in 0..depth {
        value = Value::Array(vec![value]);
    }
    value
}

#[test]
fn checkpoint_historical_golden_defaults_and_current_upgrade()
-> Result<(), Box<dyn std::error::Error>> {
    let decoded = decode_checkpoint(LEGACY)?;
    assert_eq!(decoded.source_shape, CheckpointSourceShape::LegacyDirect);
    assert_eq!(decoded.projection.workspace_id, "20200101-legacy01");
    assert_eq!(decoded.projection.started_at, "");
    let plan = decoded.projection.plan.as_ref().ok_or("missing plan")?;
    assert_eq!(plan.created_at, "");
    assert!(plan.phases[0].skills.is_empty());

    let current = encode_current_checkpoint(&decoded.projection)?;
    let value: Value = serde_json::from_slice(&current)?;
    assert_eq!(value["version"], 1);
    assert_eq!(value["payload"]["version"], 2);
    assert_eq!(value["payload"]["started_at"], "0001-01-01T00:00:00Z");
    assert_eq!(
        value["payload"]["plan"]["created_at"],
        "0001-01-01T00:00:00Z"
    );
    Ok(())
}

#[test]
fn checkpoint_direct_payload_v2_remains_a_legacy_source_shape()
-> Result<(), Box<dyn std::error::Error>> {
    let decoded = decode_checkpoint(DIRECT_V2)?;
    assert_eq!(decoded.source_shape, CheckpointSourceShape::LegacyDirect);
    assert_eq!(decoded.projection.workspace_id, "20240101-direct02");
    assert_eq!(decoded.projection.started_at, "2024-01-01T00:00:00Z");
    assert_eq!(decoded.projection.branch_name, "via/direct-v2");
    assert!(decoded.projection.plan.is_none());
    assert!(!encode_current_checkpoint(&decoded.projection)?.is_empty());
    Ok(())
}

#[test]
fn checkpoint_forward_fields_survive_preservation_and_current_round_trips()
-> Result<(), Box<dyn std::error::Error>> {
    let decoded = decode_checkpoint(FORWARD)?;
    assert_eq!(decoded.source_shape, CheckpointSourceShape::EnvelopeV1);
    let preserved = encode_preserved_checkpoint(&decoded)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&preserved)?,
        serde_json::from_slice::<Value>(FORWARD)?
    );

    let current = encode_current_checkpoint(&decoded.projection)?;
    let upgraded: Value = serde_json::from_slice(&current)?;
    assert_eq!(upgraded["envelope_future"]["keep"], true);
    assert_eq!(upgraded["payload"]["payload_future"], 17);
    assert_eq!(
        upgraded["payload"]["plan"]["plan_future"]["nested"],
        "value"
    );
    assert_eq!(
        upgraded["payload"]["plan"]["phases"][0]["phase_future"],
        json!([1, 2, 3])
    );
    assert_eq!(
        upgraded["payload"]["plan"]["phases"][0]["runtime"],
        "future-runtime"
    );
    Ok(())
}

#[test]
fn checkpoint_current_writer_has_go_field_order_and_versions()
-> Result<(), Box<dyn std::error::Error>> {
    let projection = CheckpointProjection {
        workspace_id: "20260713-current1".to_owned(),
        domain: "dev".to_owned(),
        plan: Some(CheckpointPlan {
            id: "plan-1".to_owned(),
            task: "codec".to_owned(),
            phases: vec![CheckpointPhase {
                id: "phase-1".to_owned(),
                name: "implement".to_owned(),
                objective: "encode".to_owned(),
                persona: "implementer".to_owned(),
                model_tier: "work".to_owned(),
                status: "pending".to_owned(),
                ..CheckpointPhase::default()
            }],
            execution_mode: "sequential".to_owned(),
            decomp_source: "predecomposed".to_owned(),
            created_at: "2026-07-13T00:00:00Z".to_owned(),
            ..CheckpointPlan::default()
        }),
        status: "in_progress".to_owned(),
        started_at: "2026-07-13T00:00:00Z".to_owned(),
        ..CheckpointProjection::default()
    };
    let output = String::from_utf8(encode_current_checkpoint(&projection)?)?;
    assert!(output.starts_with("{\n  \"version\": 1,\n  \"payload\": {\n    \"version\": 2,"));
    assert!(!output.ends_with('\n'));
    Ok(())
}

#[test]
fn checkpoint_negative_shapes_are_rejected() {
    let cases: &[&[u8]] = &[
        br#"{"workspace_id":""}"#,
        br#"{"version":2,"payload":{"workspace_id":"workspace"}}"#,
        br#"{"version":1,"payload":{"workspace_id":""}}"#,
        br#"{"workspace_id":"workspace","plan":[]}"#,
        br#"{"workspace_id":"workspace","started_at":""}"#,
        br#"{"workspace_id":"workspace","plan":{"created_at":""}}"#,
        b"not-json",
    ];
    for bytes in cases {
        assert!(
            decode_checkpoint(bytes).is_err(),
            "accepted {}",
            String::from_utf8_lossy(bytes)
        );
    }
}

#[test]
fn checkpoint_current_writer_rejects_go_unreadable_known_values() {
    let invalid_time = CheckpointProjection {
        workspace_id: "workspace".to_owned(),
        started_at: "not-time".to_owned(),
        ..CheckpointProjection::default()
    };
    assert!(encode_current_checkpoint(&invalid_time).is_err());

    let mut phase = CheckpointPhase::default();
    phase.extra.insert("retries".to_owned(), json!("many"));
    let invalid_phase = CheckpointProjection {
        workspace_id: "workspace".to_owned(),
        plan: Some(CheckpointPlan {
            phases: vec![phase],
            ..CheckpointPlan::default()
        }),
        ..CheckpointProjection::default()
    };
    assert!(encode_current_checkpoint(&invalid_phase).is_err());
}

#[test]
fn all_stable_event_types_and_unknown_types_decode() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(STABLE_EVENT_TYPES.len(), 45);
    for (index, event_type) in STABLE_EVENT_TYPES.iter().enumerate() {
        let line = format!(
            r#"{{"id":"evt_{index}","type":"{event_type}","timestamp":"2026-07-13T00:00:00Z","sequence":{index},"mission_id":"mission-1"}}"#
        );
        assert!(decode_event_line(line.as_bytes())?.is_stable_type);
    }
    let unknown = decode_event_line(
        br#"{"id":"evt_future","type":"future.kind","timestamp":"2026-07-13T00:00:00Z","sequence":1,"mission_id":"mission-1"}"#,
    )?;
    assert!(!unknown.is_stable_type);
    Ok(())
}

#[test]
fn event_scan_skips_corruption_keeps_file_order_and_uses_greatest_sequence()
-> Result<(), Box<dyn std::error::Error>> {
    let scan = scan_event_log(EVENTS);
    assert_eq!(scan.events.len(), 3);
    assert_eq!(scan.diagnostics.len(), 1);
    assert_eq!(scan.diagnostics[0].line_index, 2);
    assert_eq!(scan.diagnostics[0].kind, EventScanDiagnosticKind::Corrupt);
    assert_eq!(
        scan.events
            .iter()
            .map(|event| event.record.sequence)
            .collect::<Vec<_>>(),
        vec![9, 4, 7]
    );
    assert_eq!(scan.next_sequence, Some(10));
    let unknown = &scan.events[1];
    assert_eq!(
        unknown
            .record
            .extra
            .get("future_top")
            .ok_or("missing future_top")?["keep"],
        true
    );
    assert_eq!(
        unknown
            .record
            .data
            .as_ref()
            .and_then(|data| data.get("future_data"))
            .ok_or("missing future_data")?,
        &json!([1, 2])
    );
    assert_eq!(encode_preserved_event(unknown), unknown.raw_line);
    Ok(())
}

#[test]
fn event_scan_accepts_largest_go_scanner_readable_content() {
    let prefix = br#"{"id":"evt_limit","type":"mission.started","timestamp":"2026-07-13T00:00:00Z","sequence":1,"mission_id":"mission-1","data":{"padding":""#;
    let suffix = br#""}}"#;
    let mut input = Vec::with_capacity(1024 * 1024);
    input.extend_from_slice(prefix);
    input.extend(std::iter::repeat_n(
        b'x',
        GO_EVENT_JSON_CONTENT_MAX_BYTES - prefix.len() - suffix.len(),
    ));
    input.extend_from_slice(suffix);
    input.push(b'\n');

    let scan = scan_event_log(&input);
    assert_eq!(scan.events.len(), 1);
    assert!(scan.diagnostics.is_empty());
    assert_eq!(input.len(), 1024 * 1024);
}

#[test]
fn event_scan_skips_oversized_lines_and_continues() {
    let mut input = vec![b'x'; GO_EVENT_JSON_CONTENT_MAX_BYTES + 1];
    input.push(b'\n');
    input.extend_from_slice(
        br#"{"id":"evt_valid","type":"mission.started","timestamp":"2026-07-13T00:00:00Z","sequence":12,"mission_id":"mission-1"}"#,
    );
    let scan = scan_event_log(&input);
    assert_eq!(scan.events.len(), 1);
    assert_eq!(scan.diagnostics[0].kind, EventScanDiagnosticKind::Oversized);
    assert_eq!(scan.next_sequence, Some(13));
}

#[test]
fn event_scan_reports_sequence_exhaustion_without_reusing_a_sequence() {
    let line = format!(
        r#"{{"id":"evt_max","type":"mission.started","timestamp":"2026-07-13T00:00:00Z","sequence":{},"mission_id":"mission-1"}}"#,
        i64::MAX
    );
    let scan = scan_event_log(line.as_bytes());
    assert_eq!(scan.events.len(), 1);
    assert_eq!(scan.next_sequence, None);
}

#[test]
fn event_current_writer_is_compact_and_forward_tolerant() -> Result<(), Box<dyn std::error::Error>>
{
    let mut data = BTreeMap::new();
    data.insert("future".to_owned(), json!({"nested": [1, 2]}));
    let mut extra = BTreeMap::new();
    extra.insert("trace_context".to_owned(), json!("keep"));
    let event = EventRecord {
        id: "evt_0011223344556677".to_owned(),
        event_type: "future.kind".to_owned(),
        timestamp: "2026-07-13T00:00:00Z".to_owned(),
        sequence: 23,
        mission_id: "mission-1".to_owned(),
        phase_id: None,
        worker_id: None,
        data: Some(data.into()),
        extra: extra.into(),
    };
    let output = encode_current_event(&event)?;
    assert!(!output.ends_with(b"\n"));
    assert!(!output.contains(&b'\n'));
    let decoded = decode_event_line(&output)?;
    assert_eq!(
        decoded.record.extra.get("trace_context"),
        Some(&json!("keep"))
    );
    assert_eq!(decoded.record.data, event.data);
    Ok(())
}

#[test]
fn event_negative_envelopes_are_rejected() {
    let cases: &[&[u8]] = &[
        br#"{}"#,
        br#"{"id":"evt","type":"kind","timestamp":"not-time","sequence":1,"mission_id":"mission"}"#,
        br#"{"id":"evt","type":"kind","timestamp":"2026-07-13T00:00:00+24:00","sequence":1,"mission_id":"mission"}"#,
        br#"{"id":"evt","type":"kind","timestamp":"2026-07-13T00:00:00Z","sequence":1.5,"mission_id":"mission"}"#,
        br#"{"id":"evt","type":"kind","timestamp":"2026-07-13T00:00:00Z","sequence":1,"mission_id":"mission","data":[]}"#,
    ];
    for bytes in cases {
        assert!(
            decode_event_line(bytes).is_err(),
            "accepted {}",
            String::from_utf8_lossy(bytes)
        );
    }
}

#[test]
fn go_observation_decoder_accepts_partial_sequence_envelope()
-> Result<(), Box<dyn std::error::Error>> {
    let decoded = decode_go_observed_event_line(br#"{"sequence":7}"#)?;
    assert_eq!(decoded.record.sequence, 7);
    assert_eq!(decoded.record.timestamp, GO_ZERO_TIME);
    assert!(decoded.record.id.is_empty());
    assert!(decoded.record.event_type.is_empty());
    assert!(decoded.record.mission_id.is_empty());
    Ok(())
}

#[test]
fn go_observation_decoder_accepts_null_root_as_zero_event() -> Result<(), Box<dyn std::error::Error>>
{
    let decoded = decode_go_observed_event_line(b" \n null\t")?;
    assert_eq!(decoded.record.timestamp, GO_ZERO_TIME);
    assert_eq!(decoded.record.sequence, 0);
    assert!(decoded.record.id.is_empty());
    assert!(decoded.record.event_type.is_empty());
    assert!(decoded.record.mission_id.is_empty());
    assert!(decoded.record.phase_id.is_none());
    assert!(decoded.record.worker_id.is_none());
    assert!(decoded.record.data.is_none());
    assert!(decoded.record.extra.is_empty());
    assert_eq!(decoded.raw_line, b" \n null\t");
    Ok(())
}

#[test]
fn go_observation_decoder_ascii_folds_every_known_event_field()
-> Result<(), Box<dyn std::error::Error>> {
    let decoded = decode_go_observed_event_line(
        br#"{"iD":"event","tYpE":"mission.started","tImEsTaMp":"2026-07-17T01:02:03Z","sEqUeNcE":7,"mIsSiOn_Id":"mission","pHaSe_Id":"phase","wOrKeR_Id":"worker","dAtA":{"nested":{"keep":true}}}"#,
    )?;
    assert_eq!(decoded.record.id, "event");
    assert_eq!(decoded.record.event_type, "mission.started");
    assert_eq!(decoded.record.timestamp, "2026-07-17T01:02:03Z");
    assert_eq!(decoded.record.sequence, 7);
    assert_eq!(decoded.record.mission_id, "mission");
    assert_eq!(decoded.record.phase_id.as_deref(), Some("phase"));
    assert_eq!(decoded.record.worker_id.as_deref(), Some("worker"));
    assert_eq!(
        decoded
            .record
            .data
            .as_ref()
            .and_then(|data| data.get("nested")),
        Some(&json!({"keep": true}))
    );
    assert!(decoded.record.extra.is_empty());
    Ok(())
}

#[test]
fn go_observation_decoder_applies_exact_and_folded_duplicates_in_source_order()
-> Result<(), Box<dyn std::error::Error>> {
    let exact_then_folded = decode_go_observed_event_line(br#"{"sequence":1,"SEQUENCE":2}"#)?;
    assert_eq!(exact_then_folded.record.sequence, 2);

    let folded_then_exact = decode_go_observed_event_line(br#"{"SEQUENCE":2,"sequence":1}"#)?;
    assert_eq!(folded_then_exact.record.sequence, 1);

    let escaped_exact = decode_go_observed_event_line(br#"{"seque\u006ece":8}"#)?;
    assert_eq!(escaped_exact.record.sequence, 8);

    let scalar_null_is_noop = decode_go_observed_event_line(
        br#"{"id":"event","ID":null,"sequence":3,"SEQUENCE":null,"timestamp":"2026-07-17T01:02:03Z","TIMESTAMP":null}"#,
    )?;
    assert_eq!(scalar_null_is_noop.record.id, "event");
    assert_eq!(scalar_null_is_noop.record.sequence, 3);
    assert_eq!(scalar_null_is_noop.record.timestamp, "2026-07-17T01:02:03Z");
    Ok(())
}

#[test]
fn go_observation_decoder_merges_and_clears_duplicate_data_like_go()
-> Result<(), Box<dyn std::error::Error>> {
    let merged = decode_go_observed_event_line(
        br#"{"data":{"a":1,"same":"first"},"DATA":{"b":2,"same":"last"}}"#,
    )?;
    assert_eq!(
        merged.record.data,
        Some(EventJsonMap::from(BTreeMap::from([
            ("a".to_owned(), json!(1.0)),
            ("b".to_owned(), json!(2.0)),
            ("same".to_owned(), json!("last")),
        ])))
    );

    let cleared = decode_go_observed_event_line(br#"{"data":{"a":1},"DATA":null}"#)?;
    assert!(cleared.record.data.is_none());

    let replaced = decode_go_observed_event_line(br#"{"DATA":null,"data":{"b":2}}"#)?;
    assert_eq!(
        replaced.record.data,
        Some(EventJsonMap::from(BTreeMap::from([(
            "b".to_owned(),
            json!(2.0),
        )])))
    );
    Ok(())
}

#[test]
fn go_observation_decoder_matches_time_unmarshal_json_fallbacks()
-> Result<(), Box<dyn std::error::Error>> {
    // Frozen Go source fixture: time.Parse(time.RFC3339, raw) followed by
    // Time.Format(time.RFC3339Nano). Only `raw_line` keeps the input spelling.
    for (timestamp, normalized) in [
        ("2026-07-17T01:02:03,123Z", "2026-07-17T01:02:03.123Z"),
        ("2026-07-17T1:02:03Z", "2026-07-17T01:02:03Z"),
        ("2026-07-17T01:02:03+24:00", "2026-07-17T01:02:03+24:00"),
        ("2026-07-17T01:02:03-24:59", "2026-07-17T01:02:03-24:59"),
        ("2026-07-17T01:02:03+00:60", "2026-07-17T01:02:03+01:00"),
        (
            "2026-07-17T01:02:03.123456789123Z",
            "2026-07-17T01:02:03.123456789Z",
        ),
        ("2026-07-17T01:02:03.120000000Z", "2026-07-17T01:02:03.12Z"),
    ] {
        let line = format!(r#"{{"timestamp":"{timestamp}"}}"#);
        let decoded = decode_go_observed_event_line(line.as_bytes())?;
        assert_eq!(decoded.record.timestamp, normalized);
        assert_eq!(decoded.raw_line, line.as_bytes());
    }

    for line in [
        br#"{"timestamp":"2026-07-17T01:02:03\u005a"}"#.as_slice(),
        br#"{"timestamp":"2026-07-17\u005401:02:03Z"}"#.as_slice(),
        br#"{"timestamp":"2026-07-17T01:02:03+25:00"}"#.as_slice(),
        br#"{"timestamp":"2026-07-17T01:02:03+24:61"}"#.as_slice(),
    ] {
        assert!(
            decode_go_observed_event_line(line).is_err(),
            "accepted {}",
            String::from_utf8_lossy(line)
        );
    }
    Ok(())
}

#[test]
fn go_observation_decoder_uses_unicode_simple_fold_for_ascii_tags()
-> Result<(), Box<dyn std::error::Error>> {
    // encoding/json/fold.go's SimpleFold cycles place long-s with S/s and
    // Kelvin sign with K/k. Exercise both escaped and literal spellings.
    let escaped = decode_go_observed_event_line(
        br#"{"mi\u017f\u017fion_id":"escaped-mission","wor\u212aer_id":"escaped-worker"}"#,
    )?;
    assert_eq!(escaped.record.mission_id, "escaped-mission");
    assert_eq!(escaped.record.worker_id.as_deref(), Some("escaped-worker"));

    let literal = decode_go_observed_event_line(
        r#"{"miſſion_id":"literal-mission","worKer_id":"literal-worker"}"#.as_bytes(),
    )?;
    assert_eq!(literal.record.mission_id, "literal-mission");
    assert_eq!(literal.record.worker_id.as_deref(), Some("literal-worker"));
    Ok(())
}

#[test]
fn go_observation_decoder_replaces_invalid_utf8_and_utf16_in_all_string_positions()
-> Result<(), Box<dyn std::error::Error>> {
    // encoding/json/decode.go's unquoteBytes replaces each invalid UTF-8 byte
    // and each unpaired UTF-16 surrogate with U+FFFD.
    let escaped = br#"{"id":"\ud800x\udc00","data":{"\ud800":"\udc00","nested":["\ud800"]}}"#;
    let decoded = decode_go_observed_event_line(escaped)?;
    assert_eq!(decoded.record.id, "�x�");
    let data = decoded.record.data.as_ref().ok_or("missing data")?;
    assert_eq!(data.get("�"), Some(&json!("�")));
    assert_eq!(data.get("nested"), Some(&json!(["�"])));
    assert_eq!(decoded.raw_line, escaped);

    let invalid_utf8 =
        b"{\"id\":\"a\xffb\",\"data\":{\"k\xfe\":\"v\xf0\x28\x8c\x28\"},\"x\xff\":1}";
    let decoded = decode_go_observed_event_line(invalid_utf8)?;
    assert_eq!(decoded.record.id, "a�b");
    assert_eq!(
        decoded.record.data.as_ref().and_then(|data| data.get("k�")),
        Some(&json!("v�(�("))
    );
    assert_eq!(
        decoded.record.extra.get("x�").and_then(Value::as_f64),
        Some(1.0)
    );
    assert_eq!(decoded.raw_line, invalid_utf8);
    assert!(decode_event_line(invalid_utf8).is_err());
    Ok(())
}

#[test]
fn go_observation_decoder_coerces_recursive_data_numbers_to_float64()
-> Result<(), Box<dyn std::error::Error>> {
    // encoding/json/decode.go's convertNumber delegates to ParseFloat(64)
    // for interface{} values, including recursively nested map/slice data.
    let decoded = decode_go_observed_event_line(
        br#"{"data":{"rounded":9007199254740993,"negative_zero":-0,"array":[1,{"nested":2.5}]}}"#,
    )?;
    let data = decoded.record.data.as_ref().ok_or("missing data")?;
    assert_eq!(
        data.get("rounded").and_then(Value::as_f64),
        Some(9_007_199_254_740_992.0)
    );
    assert_eq!(
        data.get("negative_zero")
            .ok_or("missing negative zero")?
            .as_f64()
            .ok_or("missing negative zero")?
            .to_bits(),
        (-0.0f64).to_bits()
    );
    let array = data
        .get("array")
        .and_then(Value::as_array)
        .ok_or("missing array")?;
    assert_eq!(array[0].as_f64(), Some(1.0));
    assert_eq!(array[1]["nested"].as_f64(), Some(2.5));

    assert!(decode_go_observed_event_line(br#"{"data":{"overflow":1e400}}"#).is_err());
    let ignored_overflow = decode_go_observed_event_line(br#"{"sequence":7,"future":1e400}"#)?;
    assert_eq!(ignored_overflow.record.sequence, 7);
    assert!(!ignored_overflow.record.extra.contains_key("future"));
    Ok(())
}

#[test]
fn go_observation_decoder_materializes_go_valid_data_beyond_128_containers()
-> Result<(), Box<dyn std::error::Error>> {
    let line = nested_data_array_event(256);
    let decoded = decode_go_observed_event_line(&line)?;
    let nested = decoded
        .record
        .data
        .as_ref()
        .and_then(|data| data.get("nested"))
        .ok_or("missing nested data")?;

    assert_eq!(
        singleton_array_leaf(nested, 256).and_then(Value::as_f64),
        Some(0.0)
    );
    Ok(())
}

#[test]
fn go_observation_decoder_retains_go_valid_ignored_value_beyond_128_containers()
-> Result<(), Box<dyn std::error::Error>> {
    let line = nested_ignored_array_event(256);
    let decoded = decode_go_observed_event_line(&line)?;
    let future = decoded
        .record
        .extra
        .get("future")
        .ok_or("missing retained future value")?;

    assert_eq!(
        singleton_array_leaf(future, 256).and_then(Value::as_f64),
        Some(0.0)
    );
    Ok(())
}

#[test]
fn go_observation_decoder_accepts_data_at_10000_total_containers()
-> Result<(), Box<dyn std::error::Error>> {
    // The event root and data object consume two of Go's 10,000 containers.
    let line = nested_data_array_event(9_998);
    let decoded = decode_go_observed_event_line(&line)?;
    let nested = decoded
        .record
        .data
        .as_ref()
        .and_then(|data| data.get("nested"))
        .ok_or("missing nested data")?;

    assert_eq!(
        singleton_array_leaf(nested, 9_998).and_then(Value::as_f64),
        Some(0.0)
    );
    drop(decoded);
    Ok(())
}

#[test]
fn event_record_allows_owned_deep_field_destructuring() -> Result<(), Box<dyn std::error::Error>> {
    let line = nested_data_array_event(9_998);
    let EventRecord {
        id, data, extra, ..
    } = decode_go_observed_event_line(&line)?.record;
    let nested = data
        .as_ref()
        .and_then(|data| data.get("nested"))
        .ok_or("missing nested data")?;

    assert!(id.is_empty());
    assert_eq!(
        singleton_array_leaf(nested, 9_998).and_then(Value::as_f64),
        Some(0.0)
    );
    drop(data);
    drop(extra);
    Ok(())
}

#[test]
fn event_json_map_iteratively_replaces_and_removes_deep_values() {
    let mut values = EventJsonMap::default();
    assert!(!values.insert("nested".to_owned(), nested_array_value(9_998)));
    assert!(values.insert("nested".to_owned(), nested_array_value(9_998)));
    values.extend([("nested".to_owned(), nested_array_value(9_998))]);
    values.replace_with(BTreeMap::from([(
        "replacement".to_owned(),
        nested_array_value(9_998),
    )]));
    assert!(values.remove_and_drop("replacement"));
    assert!(!values.remove_and_drop("missing"));
    values.insert("clear".to_owned(), nested_array_value(9_998));
    values.clear();
    assert!(values.is_empty());
}

#[test]
fn event_json_map_clones_compares_and_debugs_deep_values_without_recursion() {
    let mut original = EventJsonMap::default();
    original.insert("nested".to_owned(), nested_array_value(9_998));

    let cloned = original.clone();
    assert_eq!(original, cloned);
    assert_eq!(original.len(), 1);
    assert!(format!("{original:?}").starts_with(r#"{"nested": Array ["#));
}

#[test]
fn go_observation_decoder_rejects_data_at_10001_total_containers()
-> Result<(), Box<dyn std::error::Error>> {
    // The event root and data object plus 9,999 arrays total 10,001.
    let line = nested_data_array_event(9_999);
    let error = match decode_go_observed_event_line(&line) {
        Ok(_) => return Err("accepted data at 10,001 total containers".into()),
        Err(error) => error,
    };

    assert!(error.to_string().contains("exceeded max depth"), "{error}");
    Ok(())
}

#[test]
fn go_observation_decoder_skips_ignored_value_at_10000_total_containers()
-> Result<(), Box<dyn std::error::Error>> {
    // The event root plus 9,999 ignored arrays total Go's exact limit.
    let line = nested_ignored_array_event(9_999);
    let record = decode_go_observed_event_record(&line)?;

    assert_eq!(record.sequence, 7);
    Ok(())
}

#[test]
fn go_observation_decoder_rejects_ignored_value_at_10001_total_containers()
-> Result<(), Box<dyn std::error::Error>> {
    // The event root plus 10,000 ignored arrays exceed Go's limit by one.
    let line = nested_ignored_array_event(10_000);
    let error = match decode_go_observed_event_record(&line) {
        Ok(_) => return Err("accepted ignored value at 10,001 total containers".into()),
        Err(error) => error,
    };

    assert!(error.to_string().contains("exceeded max depth"), "{error}");
    Ok(())
}

#[test]
fn go_observation_decoder_keeps_strict_admission_decoder_strict() {
    assert!(decode_go_observed_event_line(br#"{"sequence":7}"#).is_ok());
    assert!(decode_event_line(br#"{"sequence":7}"#).is_err());
}

#[test]
fn go_json_object_members_preserve_duplicates_raw_values_and_invalid_utf8()
-> Result<(), Box<dyn std::error::Error>> {
    let bytes = b"{\"name\":\"first\",\"NAME\":\"bad-\xff\"}";
    let members = decode_go_json_object_members(bytes)?
        .ok_or("object projection unexpectedly returned null")?;

    assert_eq!(members.len(), 2);
    assert_eq!(members[0].name, "name");
    assert_eq!(members[0].raw_value, br#""first""#);
    assert_eq!(members[1].name, "NAME");
    let GoJsonScalar::String {
        decoded,
        raw_content,
    } = decode_go_json_scalar(members[1].raw_value)?
    else {
        return Err("second member was not a string".into());
    };
    assert_eq!(decoded, "bad-\u{fffd}");
    assert_eq!(raw_content, b"bad-\xff");
    Ok(())
}

#[test]
fn go_json_array_and_scalar_views_preserve_source_tokens() -> Result<(), Box<dyn std::error::Error>>
{
    let elements = decode_go_json_array_elements(br#"[null, "x", -7]"#)?
        .ok_or("array projection unexpectedly returned null")?;

    assert_eq!(
        elements,
        vec![b"null".as_slice(), b"\"x\"".as_slice(), b"-7".as_slice()]
    );
    assert_eq!(decode_go_json_scalar(elements[0])?, GoJsonScalar::Null);
    assert_eq!(
        decode_go_json_scalar(elements[2])?,
        GoJsonScalar::Number("-7")
    );
    Ok(())
}

#[test]
fn go_observation_decoder_applies_null_zero_values_and_preserves_unknowns()
-> Result<(), Box<dyn std::error::Error>> {
    let decoded = decode_go_observed_event_line(
        br#"{"id":null,"type":null,"timestamp":null,"sequence":null,"mission_id":null,"phase_id":null,"worker_id":null,"data":null,"future":{"keep":true}}"#,
    )?;
    assert_eq!(decoded.record.timestamp, GO_ZERO_TIME);
    assert_eq!(decoded.record.sequence, 0);
    assert!(decoded.record.phase_id.is_none());
    assert!(decoded.record.worker_id.is_none());
    assert_eq!(
        decoded.record.extra.get("future"),
        Some(&json!({"keep": true}))
    );
    Ok(())
}

#[test]
fn go_observation_decoder_rejects_wrong_typed_known_fields() {
    let cases: &[&[u8]] = &[
        br#"{"id":1}"#,
        br#"{"type":false}"#,
        br#"{"timestamp":"not-time"}"#,
        br#"{"timestamp":1}"#,
        br#"{"sequence":7.0}"#,
        br#"{"mission_id":[]}"#,
        br#"{"phase_id":{}}"#,
        br#"{"worker_id":1}"#,
        br#"{"data":[]}"#,
        br#"{"ID":1}"#,
        br#"{"TYPE":false}"#,
        br#"{"TIMESTAMP":1}"#,
        br#"{"SEQUENCE":"seven"}"#,
        br#"{"MISSION_ID":[]}"#,
        br#"{"PHASE_ID":{}}"#,
        br#"{"WORKER_ID":1}"#,
        br#"{"DATA":[]}"#,
    ];
    for bytes in cases {
        assert!(
            decode_go_observed_event_line(bytes).is_err(),
            "accepted {}",
            String::from_utf8_lossy(bytes)
        );
    }
}
