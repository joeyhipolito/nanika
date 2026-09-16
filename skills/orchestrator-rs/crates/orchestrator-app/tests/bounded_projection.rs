use orchestrator_app::project_event;
use orchestrator_core::{
    CoreError, EventId, MissionId, MissionState, PhaseDefinition, PhaseId, ReducerTransition,
    TransitionError, decode_event_line, encode_preserved_event, reduce,
};

#[test]
fn decoded_events_project_lifecycle_and_unknown_records_without_rewriting()
-> Result<(), Box<dyn std::error::Error>> {
    let failed = decode_event_line(
        br#"{"id":"evt_failed","type":"phase.failed","timestamp":"2026-07-13T00:00:00Z","sequence":4,"mission_id":"mission-1","phase_id":"phase-1","data":{"error":"boom","future":true}}"#,
    )?;
    let input = project_event(&failed)?;
    assert!(matches!(
        input.transition,
        ReducerTransition::PhaseFailed { error } if error == "boom"
    ));
    assert_eq!(input.data["future"], true);

    let unknown = decode_event_line(
        br#"{"id":"evt_future","type":"future.kind","timestamp":"2026-07-13T00:00:01Z","sequence":9,"mission_id":"mission-1","data":{"future":[1,2]}}"#,
    )?;
    let input = project_event(&unknown)?;
    assert!(matches!(
        input.transition,
        ReducerTransition::Unknown { event_type, data }
            if event_type == "future.kind" && data["future"] == serde_json::json!([1, 2])
    ));
    Ok(())
}

#[test]
fn persisted_event_projection_and_serde_accept_colon_id_while_live_creation_remains_strict()
-> Result<(), Box<dyn std::error::Error>> {
    let source = br#"{"id":"legacy.event:1","type":"future.kind","timestamp":"2026-07-13T00:00:00Z","sequence":1,"mission_id":"mission-1","data":{"future":true},"future_top":{"keep":true}}"#;
    let decoded = decode_event_line(source)?;
    let input = project_event(&decoded)?;

    assert_eq!(input.event_id.as_str(), "legacy.event:1");
    let serialized = serde_json::to_string(&input.event_id)?;
    assert_eq!(serialized, r#""legacy.event:1""#);
    let deserialized: EventId = serde_json::from_str(&serialized)?;
    assert_eq!(deserialized, input.event_id);
    assert_eq!(input.data["future"], true);
    assert_eq!(input.extra["future_top"]["keep"], true);
    assert_eq!(encode_preserved_event(&decoded).as_slice(), source);
    assert!(matches!(
        EventId::new("legacy.event:1"),
        Err(CoreError::InvalidIdentifier { kind: "event", .. })
    ));
    Ok(())
}

#[test]
fn persisted_event_projection_rejects_empty_forged_event_id()
-> Result<(), Box<dyn std::error::Error>> {
    let mut decoded = decode_event_line(
        br#"{"id":"evt_valid","type":"mission.started","timestamp":"2026-07-13T00:00:00Z","sequence":1,"mission_id":"mission-1"}"#,
    )?;
    decoded.record.id.clear();

    assert!(matches!(
        project_event(&decoded),
        Err(CoreError::InvalidIdentifier {
            kind: "event",
            reason: "identifier is empty",
            ..
        })
    ));
    Ok(())
}

#[test]
fn persisted_event_projection_rejects_control_character_event_id_without_rewriting_source()
-> Result<(), Box<dyn std::error::Error>> {
    let source = br#"{"id":"legacy\u0000event","type":"mission.started","timestamp":"2026-07-13T00:00:00Z","sequence":1,"mission_id":"mission-1"}"#;
    let decoded = decode_event_line(source)?;

    assert!(matches!(
        project_event(&decoded),
        Err(CoreError::InvalidIdentifier {
            kind: "event",
            reason: "persisted event identifier contains a control character",
            ..
        })
    ));
    assert_eq!(encode_preserved_event(&decoded).as_slice(), source);
    Ok(())
}

#[test]
fn persisted_event_projection_rejects_event_id_over_256_bytes()
-> Result<(), Box<dyn std::error::Error>> {
    let source = serde_json::to_vec(&serde_json::json!({
        "id": "x".repeat(257),
        "type": "mission.started",
        "timestamp": "2026-07-13T00:00:00Z",
        "sequence": 1,
        "mission_id": "mission-1"
    }))?;
    let decoded = decode_event_line(&source)?;

    assert!(matches!(
        project_event(&decoded),
        Err(CoreError::InvalidIdentifier {
            kind: "event",
            reason: "persisted event identifier exceeds 256 bytes",
            ..
        })
    ));
    assert_eq!(encode_preserved_event(&decoded), source);
    Ok(())
}

#[test]
fn projected_replay_fingerprint_retains_data_extra_and_worker_independently()
-> Result<(), Box<dyn std::error::Error>> {
    let cases: [(&str, &[u8], &[u8]); 3] = [
        (
            "data",
            br#"{"id":"evt_start","type":"mission.started","timestamp":"2026-07-13T00:00:00Z","sequence":1,"mission_id":"mission-1","worker_id":"worker-1","data":{"future":1},"future_top":"alpha"}"#,
            br#"{"id":"evt_start","type":"mission.started","timestamp":"2026-07-13T00:00:00Z","sequence":1,"mission_id":"mission-1","worker_id":"worker-1","data":{"future":2},"future_top":"alpha"}"#,
        ),
        (
            "extra",
            br#"{"id":"evt_start","type":"mission.started","timestamp":"2026-07-13T00:00:00Z","sequence":1,"mission_id":"mission-1","worker_id":"worker-1","data":{"future":1},"future_top":"alpha"}"#,
            br#"{"id":"evt_start","type":"mission.started","timestamp":"2026-07-13T00:00:00Z","sequence":1,"mission_id":"mission-1","worker_id":"worker-1","data":{"future":1},"future_top":"beta"}"#,
        ),
        (
            "worker",
            br#"{"id":"evt_start","type":"mission.started","timestamp":"2026-07-13T00:00:00Z","sequence":1,"mission_id":"mission-1","worker_id":"worker-1","data":{"future":1},"future_top":"alpha"}"#,
            br#"{"id":"evt_start","type":"mission.started","timestamp":"2026-07-13T00:00:00Z","sequence":1,"mission_id":"mission-1","worker_id":"worker-2","data":{"future":1},"future_top":"alpha"}"#,
        ),
    ];
    for (field, original, conflicting) in cases {
        let original = project_event(&decode_event_line(original)?)?;
        let conflicting = project_event(&decode_event_line(conflicting)?)?;
        assert_eq!(original.data["future"], 1, "{field}");
        assert_eq!(original.extra["future_top"], "alpha", "{field}");
        assert_eq!(original.worker_id.as_deref(), Some("worker-1"), "{field}");

        let state = MissionState::new(
            MissionId::new("mission-1")?,
            vec![PhaseDefinition {
                id: PhaseId::new("phase-1")?,
                dependencies: Vec::new(),
            }],
        )?;
        let state = reduce(&state, &original)?.state;
        assert_eq!(
            reduce(&state, &conflicting),
            Err(TransitionError::EventIdentityConflict {
                event_id: original.event_id.clone(),
            }),
            "{field} changed"
        );
    }
    Ok(())
}
