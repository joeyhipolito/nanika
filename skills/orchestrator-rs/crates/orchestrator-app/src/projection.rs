use orchestrator_core::{
    CoreError, DecodedEvent, MissionId, PhaseId, ReducerInput, ReducerTransition,
};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Converts a decoded compatibility event into the pure reducer input vocabulary.
/// Stable non-lifecycle and unknown types become ordering-only observations.
pub fn project_event(event: &DecodedEvent) -> Result<ReducerInput, CoreError> {
    let record = &event.record;
    let data = record
        .data
        .as_ref()
        .map(|values| {
            Value::Object(
                values
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<Map<_, _>>(),
            )
        })
        .unwrap_or(Value::Null);
    let transition = match record.event_type.as_str() {
        "mission.started" => ReducerTransition::MissionStarted,
        "mission.completed" => ReducerTransition::MissionCompleted,
        "mission.failed" => ReducerTransition::MissionFailed,
        "mission.cancelled" => ReducerTransition::MissionCancelled {
            reason: data_string(&data, "reason"),
        },
        "phase.started" => ReducerTransition::PhaseStarted,
        "phase.completed" => ReducerTransition::PhaseCompleted,
        "phase.failed" => ReducerTransition::PhaseFailed {
            error: data_string(&data, "error"),
        },
        "phase.skipped" => ReducerTransition::PhaseSkipped {
            reason: data_string(&data, "reason"),
        },
        "phase.retrying" => ReducerTransition::PhaseRetrying,
        _ => ReducerTransition::Unknown {
            event_type: record.event_type.clone(),
            data: data.clone(),
        },
    };
    Ok(ReducerInput {
        event_id: event.persisted_event_id()?,
        sequence: record.sequence,
        timestamp: record.timestamp.clone(),
        mission_id: MissionId::new(record.mission_id.clone())?,
        phase_id: record.phase_id.clone().map(PhaseId::new).transpose()?,
        worker_id: record.worker_id.clone(),
        data,
        extra: record
            .extra
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>(),
        transition,
    })
}

fn data_string(data: &Value, key: &str) -> String {
    data.as_object()
        .and_then(|object| object.get(key))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}
