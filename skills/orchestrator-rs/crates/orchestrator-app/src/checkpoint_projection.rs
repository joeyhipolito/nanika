//! Shared plan/checkpoint status projection.
//!
//! This is the single place that maps a reducer [`MissionState`] onto the
//! Go-compatible checkpoint status vocabulary. The fixture lifecycle
//! coordinator (`lifecycle.rs`) is the only current caller; a future
//! hermetic compatibility projector (Cell 2D) must call the same mapping
//! instead of re-deriving mission/phase status strings, so the two writers
//! cannot drift apart on checkpoint-plan vocabulary.

use crate::lifecycle::LifecycleError;
use orchestrator_core::{
    CheckpointProjection, MissionId, MissionState, MissionStatus, PhaseStatus,
    encode_current_checkpoint,
};
use serde_json::Value;

/// Extra-map key for the historical fixture-only in-checkpoint event-sequence
/// bookmark. Present only in checkpoints that already opted in; never added
/// by this module to a checkpoint that lacks it.
pub(crate) const FIXTURE_EVENT_SEQUENCE_KEY: &str = "fixture_event_sequence";

/// Journal-derived proof of the exact checkpoint bytes recovery may adopt.
///
/// The private fields and sole factory keep raw disk bytes or caller guesses
/// from being upgraded into recovery authority.
pub(crate) struct JournalCheckpointExpectation {
    mission_id: MissionId,
    checkpoint_bytes: Vec<u8>,
}

impl JournalCheckpointExpectation {
    pub(crate) const fn mission_id(&self) -> &MissionId {
        &self.mission_id
    }

    pub(crate) fn checkpoint_bytes(&self) -> &[u8] {
        &self.checkpoint_bytes
    }
}

/// Projects `state`'s mission/phase status onto a clone of `template`.
///
/// The returned checkpoint's `FIXTURE_EVENT_SEQUENCE_KEY` presence is fully
/// determined by `template` and `state`: present only when `template`
/// already carried the key, and then only if `state` has an applied
/// sequence to record. Callers must not re-check or re-clean that key after
/// calling this function — the invariant already holds and doing so again
/// is a no-op.
pub(crate) fn checkpoint_for_state(
    template: &CheckpointProjection,
    state: &MissionState,
) -> Result<CheckpointProjection, LifecycleError> {
    let mut checkpoint = template.clone();
    if template.extra.contains_key(FIXTURE_EVENT_SEQUENCE_KEY) {
        match state.greatest_applied_sequence() {
            Some(sequence) => {
                checkpoint
                    .extra
                    .insert(FIXTURE_EVENT_SEQUENCE_KEY.to_owned(), Value::from(sequence));
            }
            None => {
                checkpoint.extra.remove(FIXTURE_EVENT_SEQUENCE_KEY);
            }
        }
    }
    checkpoint.status = mission_status_name(state.status()).to_owned();
    checkpoint.started_at = state
        .started_at()
        .map(str::to_owned)
        .unwrap_or_else(|| orchestrator_core::GO_ZERO_TIME.to_owned());
    let plan = checkpoint
        .plan
        .as_mut()
        .ok_or(LifecycleError::CheckpointPlanMismatch)?;
    for phase in state.phases() {
        let projected = plan
            .phases
            .iter_mut()
            .find(|candidate| candidate.id == phase.id.as_str())
            .ok_or(LifecycleError::CheckpointPlanMismatch)?;
        projected.status = phase_status_name(phase.status).to_owned();
    }
    Ok(checkpoint)
}

/// Seals a checkpoint only after proving it is the canonical fixed point for
/// the supplied journal-reduced mission state.
pub(crate) fn journal_checkpoint_expectation(
    checkpoint: &CheckpointProjection,
    state: &MissionState,
) -> Result<JournalCheckpointExpectation, LifecycleError> {
    if checkpoint.workspace_id != state.mission_id().as_str() {
        return Err(LifecycleError::MissionMismatch);
    }
    if checkpoint_for_state(checkpoint, state)? != *checkpoint {
        return Err(LifecycleError::RecoveryConflict);
    }
    Ok(JournalCheckpointExpectation {
        mission_id: state.mission_id().clone(),
        checkpoint_bytes: encode_current_checkpoint(checkpoint)?,
    })
}

/// True when `checkpoint` already reflects `state` under [`checkpoint_for_state`].
pub(crate) fn checkpoint_matches_state(
    checkpoint: &CheckpointProjection,
    state: &MissionState,
) -> bool {
    checkpoint_for_state(checkpoint, state).is_ok_and(|expected| &expected == checkpoint)
}

pub(crate) const fn mission_status_name(status: MissionStatus) -> &'static str {
    match status {
        MissionStatus::NotStarted => "pending",
        MissionStatus::InProgress => "in_progress",
        MissionStatus::Completed => "completed",
        MissionStatus::Failed => "failed",
        MissionStatus::Cancelled => "cancelled",
    }
}

pub(crate) const fn phase_status_name(status: PhaseStatus) -> &'static str {
    match status {
        PhaseStatus::Pending => "pending",
        PhaseStatus::Running => "running",
        PhaseStatus::Completed => "completed",
        PhaseStatus::Failed => "failed",
        PhaseStatus::Skipped => "skipped",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FIXTURE_EVENT_SEQUENCE_KEY, checkpoint_for_state, checkpoint_matches_state,
        journal_checkpoint_expectation,
    };
    use crate::lifecycle::LifecycleError;
    use orchestrator_core::{
        CheckpointPhase, CheckpointPlan, CheckpointProjection, MissionId, MissionState,
        PhaseDefinition, PhaseId, encode_current_checkpoint,
    };
    use serde_json::json;

    fn plan_projection(phase_ids: &[&str]) -> CheckpointProjection {
        CheckpointProjection {
            workspace_id: "20260717-checkpoint-projection".to_owned(),
            plan: Some(CheckpointPlan {
                phases: phase_ids
                    .iter()
                    .map(|id| CheckpointPhase {
                        id: (*id).to_owned(),
                        ..CheckpointPhase::default()
                    })
                    .collect(),
                ..CheckpointPlan::default()
            }),
            ..CheckpointProjection::default()
        }
    }

    fn pristine_state(phase_ids: &[&str]) -> Result<MissionState, Box<dyn std::error::Error>> {
        let mission_id = MissionId::new("20260717-checkpoint-projection")?;
        let definitions = phase_ids
            .iter()
            .map(|id| {
                Ok(PhaseDefinition {
                    id: PhaseId::new(*id)?,
                    dependencies: Vec::new(),
                })
            })
            .collect::<Result<Vec<_>, orchestrator_core::CoreError>>()?;
        Ok(MissionState::new(mission_id, definitions)?)
    }

    #[test]
    fn fixture_event_sequence_key_absent_when_template_never_opted_in()
    -> Result<(), Box<dyn std::error::Error>> {
        let template = plan_projection(&["phase-1"]);
        assert!(!template.extra.contains_key(FIXTURE_EVENT_SEQUENCE_KEY));
        let state = pristine_state(&["phase-1"])?;
        let checkpoint = checkpoint_for_state(&template, &state)?;
        assert!(!checkpoint.extra.contains_key(FIXTURE_EVENT_SEQUENCE_KEY));
        Ok(())
    }

    #[test]
    fn fixture_event_sequence_key_removed_when_no_sequence_applied_yet()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut template = plan_projection(&["phase-1"]);
        template
            .extra
            .insert(FIXTURE_EVENT_SEQUENCE_KEY.to_owned(), json!(3));
        let state = pristine_state(&["phase-1"])?;
        assert!(state.greatest_applied_sequence().is_none());
        let checkpoint = checkpoint_for_state(&template, &state)?;
        assert!(!checkpoint.extra.contains_key(FIXTURE_EVENT_SEQUENCE_KEY));
        Ok(())
    }

    #[test]
    fn checkpoint_plan_mismatch_when_state_phase_is_absent_from_plan()
    -> Result<(), Box<dyn std::error::Error>> {
        let template = plan_projection(&["phase-1"]);
        let state = pristine_state(&["phase-1", "phase-2"])?;
        assert!(checkpoint_for_state(&template, &state).is_err());
        Ok(())
    }

    #[test]
    fn matches_state_is_true_for_a_checkpoint_already_at_fixed_point()
    -> Result<(), Box<dyn std::error::Error>> {
        let template = plan_projection(&["phase-1"]);
        let state = pristine_state(&["phase-1"])?;
        let checkpoint = checkpoint_for_state(&template, &state)?;
        assert!(checkpoint_matches_state(&checkpoint, &state));
        Ok(())
    }

    #[test]
    fn journal_expectation_captures_exact_canonical_bytes_and_mission_at_fixed_point()
    -> Result<(), Box<dyn std::error::Error>> {
        let state = pristine_state(&["phase-1"])?;
        let checkpoint = checkpoint_for_state(&plan_projection(&["phase-1"]), &state)?;
        let expected_bytes = encode_current_checkpoint(&checkpoint)?;
        let expectation = journal_checkpoint_expectation(&checkpoint, &state)?;

        assert_eq!(expectation.mission_id(), state.mission_id());
        assert_eq!(expectation.checkpoint_bytes(), expected_bytes.as_slice());
        Ok(())
    }

    #[test]
    fn journal_expectation_rejects_a_checkpoint_for_another_mission()
    -> Result<(), Box<dyn std::error::Error>> {
        let state = pristine_state(&["phase-1"])?;
        let mut checkpoint = checkpoint_for_state(&plan_projection(&["phase-1"]), &state)?;
        checkpoint.workspace_id = "another-mission".to_owned();

        assert!(matches!(
            journal_checkpoint_expectation(&checkpoint, &state),
            Err(LifecycleError::MissionMismatch)
        ));
        Ok(())
    }

    #[test]
    fn journal_expectation_rejects_same_mission_checkpoint_that_is_not_a_fixed_point()
    -> Result<(), Box<dyn std::error::Error>> {
        let state = pristine_state(&["phase-1"])?;
        let mut checkpoint = checkpoint_for_state(&plan_projection(&["phase-1"]), &state)?;
        checkpoint.status = "in_progress".to_owned();

        assert!(matches!(
            journal_checkpoint_expectation(&checkpoint, &state),
            Err(LifecycleError::RecoveryConflict)
        ));
        Ok(())
    }
}
