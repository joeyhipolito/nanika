//! Pure mission and phase lifecycle reduction.

use crate::{EventId, MissionId, PhaseId};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// A phase and its dependency edges, in authored plan order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhaseDefinition {
    pub id: PhaseId,
    pub dependencies: Vec<PhaseId>,
}

/// Mission lifecycle status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissionStatus {
    NotStarted,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

impl MissionStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Phase lifecycle status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhaseStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
}

impl PhaseStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Skipped)
    }
}

/// Reduced state for one planned phase.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhaseState {
    pub id: PhaseId,
    pub dependencies: Vec<PhaseId>,
    pub status: PhaseStatus,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub error: Option<String>,
    pub skip_reason: Option<String>,
    pub retry_observations: u64,
}

/// A typed lifecycle transition. Unknown records are ordering-only observations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReducerTransition {
    MissionStarted,
    MissionCompleted,
    MissionFailed,
    MissionCancelled { reason: String },
    PhaseStarted,
    PhaseCompleted,
    PhaseFailed { error: String },
    PhaseSkipped { reason: String },
    PhaseRetrying,
    Unknown { event_type: String, data: Value },
}

impl ReducerTransition {
    const fn is_lifecycle(&self) -> bool {
        !matches!(self, Self::Unknown { .. })
    }

    const fn requires_phase(&self) -> bool {
        matches!(
            self,
            Self::PhaseStarted
                | Self::PhaseCompleted
                | Self::PhaseFailed { .. }
                | Self::PhaseSkipped { .. }
                | Self::PhaseRetrying
        )
    }
}

/// One already-applied event fingerprint, retained for replay detection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedEvent {
    pub sequence: i64,
    pub timestamp: String,
    pub mission_id: MissionId,
    pub phase_id: Option<PhaseId>,
    /// Worker attribution retained for same-ID replay conflict detection.
    pub worker_id: Option<String>,
    /// Complete event data retained for same-ID replay conflict detection.
    pub data: Value,
    /// Unknown top-level envelope fields retained for same-ID replay conflict detection.
    pub extra: BTreeMap<String, Value>,
    pub transition: ReducerTransition,
}

/// Fully explicit input to the reducer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReducerInput {
    pub event_id: EventId,
    pub sequence: i64,
    pub timestamp: String,
    pub mission_id: MissionId,
    pub phase_id: Option<PhaseId>,
    /// Worker attribution from the event envelope.
    pub worker_id: Option<String>,
    /// Complete event data, including fields unrelated to the lifecycle transition.
    pub data: Value,
    /// Unknown top-level fields from the event envelope.
    pub extra: BTreeMap<String, Value>,
    pub transition: ReducerTransition,
}

impl ReducerInput {
    fn fingerprint(&self) -> AppliedEvent {
        AppliedEvent {
            sequence: self.sequence,
            timestamp: self.timestamp.clone(),
            mission_id: self.mission_id.clone(),
            phase_id: self.phase_id.clone(),
            worker_id: self.worker_id.clone(),
            data: self.data.clone(),
            extra: self.extra.clone(),
            transition: self.transition.clone(),
        }
    }
}

/// Canonical state reduced for a single mission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissionState {
    mission_id: MissionId,
    status: MissionStatus,
    started_at: Option<String>,
    finished_at: Option<String>,
    phase_order: Vec<PhaseId>,
    phases: BTreeMap<PhaseId, PhaseState>,
    greatest_applied_sequence: Option<i64>,
    applied_events: BTreeMap<EventId, AppliedEvent>,
    applied_sequences: BTreeMap<i64, EventId>,
}

impl MissionState {
    /// Builds an initial state and validates the complete dependency DAG.
    pub fn new(
        mission_id: MissionId,
        definitions: Vec<PhaseDefinition>,
    ) -> Result<Self, MissionStateBuildError> {
        if definitions.is_empty() {
            return Err(MissionStateBuildError::EmptyPlan);
        }
        let known = definitions
            .iter()
            .map(|definition| definition.id.clone())
            .collect::<BTreeSet<_>>();
        if known.len() != definitions.len() {
            let mut seen = BTreeSet::new();
            let duplicate = definitions
                .iter()
                .find(|definition| !seen.insert(definition.id.clone()))
                .map(|definition| definition.id.clone());
            return match duplicate {
                Some(phase_id) => Err(MissionStateBuildError::DuplicatePhase { phase_id }),
                None => Err(MissionStateBuildError::EmptyPlan),
            };
        }
        for definition in &definitions {
            let mut dependencies = BTreeSet::new();
            for dependency in &definition.dependencies {
                if !known.contains(dependency) {
                    return Err(MissionStateBuildError::UnknownDependency {
                        phase_id: definition.id.clone(),
                        dependency: dependency.clone(),
                    });
                }
                if !dependencies.insert(dependency.clone()) {
                    return Err(MissionStateBuildError::DuplicateDependency {
                        phase_id: definition.id.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
        }
        detect_cycle(&definitions)?;

        let phase_order = definitions
            .iter()
            .map(|definition| definition.id.clone())
            .collect();
        let phases = definitions
            .into_iter()
            .map(|definition| {
                let id = definition.id;
                (
                    id.clone(),
                    PhaseState {
                        id,
                        dependencies: definition.dependencies,
                        status: PhaseStatus::Pending,
                        started_at: None,
                        finished_at: None,
                        error: None,
                        skip_reason: None,
                        retry_observations: 0,
                    },
                )
            })
            .collect();
        Ok(Self {
            mission_id,
            status: MissionStatus::NotStarted,
            started_at: None,
            finished_at: None,
            phase_order,
            phases,
            greatest_applied_sequence: None,
            applied_events: BTreeMap::new(),
            applied_sequences: BTreeMap::new(),
        })
    }

    #[must_use]
    pub const fn mission_id(&self) -> &MissionId {
        &self.mission_id
    }

    #[must_use]
    pub const fn status(&self) -> MissionStatus {
        self.status
    }

    #[must_use]
    pub fn started_at(&self) -> Option<&str> {
        self.started_at.as_deref()
    }

    #[must_use]
    pub fn finished_at(&self) -> Option<&str> {
        self.finished_at.as_deref()
    }

    #[must_use]
    pub fn phase(&self, phase_id: &PhaseId) -> Option<&PhaseState> {
        self.phases.get(phase_id)
    }

    /// Iterates phases in authored plan order.
    pub fn phases(&self) -> impl Iterator<Item = &PhaseState> {
        self.phase_order
            .iter()
            .filter_map(|phase_id| self.phases.get(phase_id))
    }

    #[must_use]
    pub const fn greatest_applied_sequence(&self) -> Option<i64> {
        self.greatest_applied_sequence
    }

    #[must_use]
    pub fn applied_event(&self, event_id: &EventId) -> Option<&AppliedEvent> {
        self.applied_events.get(event_id)
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum MissionStateBuildError {
    #[error("a mission plan must contain at least one phase")]
    EmptyPlan,
    #[error("duplicate phase {phase_id}")]
    DuplicatePhase { phase_id: PhaseId },
    #[error("phase {phase_id} repeats dependency {dependency}")]
    DuplicateDependency {
        phase_id: PhaseId,
        dependency: PhaseId,
    },
    #[error("phase {phase_id} has unknown dependency {dependency}")]
    UnknownDependency {
        phase_id: PhaseId,
        dependency: PhaseId,
    },
    #[error("dependency cycle includes phase {phase_id}")]
    DependencyCycle { phase_id: PhaseId },
}

/// A successful pure reduction and any phases newly eligible for dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reduction {
    pub state: MissionState,
    pub released_phases: Vec<PhaseId>,
}

/// Typed reasons a lifecycle event cannot be applied.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum TransitionError {
    #[error("event id {event_id} was reused with different content")]
    EventIdentityConflict { event_id: EventId },
    #[error("sequence {sequence} is already owned by event {existing_event_id}")]
    SequenceConflict {
        sequence: i64,
        existing_event_id: EventId,
    },
    #[error("sequence {sequence} is older than greatest applied sequence {greatest}")]
    OutOfOrder { sequence: i64, greatest: i64 },
    #[error("event mission {found} does not match reducer mission {expected}")]
    MissionMismatch {
        expected: MissionId,
        found: MissionId,
    },
    #[error("mission is already started")]
    AlreadyStarted,
    #[error("mission is terminal in state {status:?}")]
    MissionTerminal { status: MissionStatus },
    #[error("mission must be in progress; current state is {status:?}")]
    MissionNotInProgress { status: MissionStatus },
    #[error("mission cannot complete while phases remain incomplete")]
    MissionPhasesIncomplete,
    #[error("mission cannot complete after a phase failed")]
    MissionHasFailedPhase,
    #[error("phase lifecycle event has no phase id")]
    MissingPhaseId,
    #[error("unknown phase {phase_id}")]
    UnknownPhase { phase_id: PhaseId },
    #[error("phase {phase_id} is already running")]
    PhaseAlreadyRunning { phase_id: PhaseId },
    #[error("phase {phase_id} is not running; current state is {status:?}")]
    PhaseNotRunning {
        phase_id: PhaseId,
        status: PhaseStatus,
    },
    #[error("phase {phase_id} is terminal in state {status:?}")]
    PhaseTerminal {
        phase_id: PhaseId,
        status: PhaseStatus,
    },
    #[error("phase {phase_id} dependency {dependency} is not ready")]
    DependencyNotReady {
        phase_id: PhaseId,
        dependency: PhaseId,
    },
    /// Defensive rejection for a future validated projected state containing a
    /// pending phase whose dependency is terminal. Fresh reduced histories
    /// eagerly skip such dependents, so terminal-phase monotonicity wins there.
    #[error("phase {phase_id} dependency {dependency} ended in state {status:?}")]
    DependencyFailed {
        phase_id: PhaseId,
        dependency: PhaseId,
        status: PhaseStatus,
    },
    #[error("phase {phase_id} retry observation count overflowed")]
    RetryCountOverflow { phase_id: PhaseId },
}

/// Applies one event without I/O, external effects, persistence, retries, or metrics.
pub fn reduce(state: &MissionState, input: &ReducerInput) -> Result<Reduction, TransitionError> {
    let fingerprint = input.fingerprint();
    if let Some(applied) = state.applied_events.get(&input.event_id) {
        return if applied == &fingerprint {
            Ok(Reduction {
                state: state.clone(),
                released_phases: Vec::new(),
            })
        } else {
            Err(TransitionError::EventIdentityConflict {
                event_id: input.event_id.clone(),
            })
        };
    }
    if let Some(existing_event_id) = state.applied_sequences.get(&input.sequence) {
        return Err(TransitionError::SequenceConflict {
            sequence: input.sequence,
            existing_event_id: existing_event_id.clone(),
        });
    }
    if let Some(greatest) = state.greatest_applied_sequence {
        if input.sequence < greatest {
            return Err(TransitionError::OutOfOrder {
                sequence: input.sequence,
                greatest,
            });
        }
    }
    if input.mission_id != state.mission_id {
        return Err(TransitionError::MissionMismatch {
            expected: state.mission_id.clone(),
            found: input.mission_id.clone(),
        });
    }

    let mut next = state.clone();
    let released_phases = apply_transition(&mut next, input)?;
    next.greatest_applied_sequence = Some(input.sequence);
    next.applied_sequences
        .insert(input.sequence, input.event_id.clone());
    next.applied_events
        .insert(input.event_id.clone(), fingerprint);
    Ok(Reduction {
        state: next,
        released_phases,
    })
}

fn apply_transition(
    state: &mut MissionState,
    input: &ReducerInput,
) -> Result<Vec<PhaseId>, TransitionError> {
    if state.status.is_terminal() && input.transition.is_lifecycle() {
        return Err(TransitionError::MissionTerminal {
            status: state.status,
        });
    }
    if input.transition.requires_phase() {
        let phase_id = input
            .phase_id
            .as_ref()
            .ok_or(TransitionError::MissingPhaseId)?;
        if !state.phases.contains_key(phase_id) {
            return Err(TransitionError::UnknownPhase {
                phase_id: phase_id.clone(),
            });
        }
    }

    match &input.transition {
        ReducerTransition::MissionStarted => {
            if state.status != MissionStatus::NotStarted {
                return Err(TransitionError::AlreadyStarted);
            }
            state.status = MissionStatus::InProgress;
            state.started_at = Some(input.timestamp.clone());
        }
        ReducerTransition::MissionCompleted => complete_mission(state, &input.timestamp)?,
        ReducerTransition::MissionFailed => {
            require_in_progress(state)?;
            state.status = MissionStatus::Failed;
            state.finished_at = Some(input.timestamp.clone());
            skip_all_active(state, &input.timestamp, "mission failed");
        }
        ReducerTransition::MissionCancelled { reason } => {
            state.status = MissionStatus::Cancelled;
            state.finished_at = Some(input.timestamp.clone());
            skip_all_active(state, &input.timestamp, reason);
        }
        ReducerTransition::PhaseStarted => start_phase(state, input)?,
        ReducerTransition::PhaseCompleted => return complete_phase(state, input),
        ReducerTransition::PhaseFailed { error } => {
            finish_phase(state, input, PhaseStatus::Failed, Some(error.clone()), None)?;
            let phase_id = required_phase_id(input)?;
            skip_pending_dependents(
                state,
                phase_id,
                &input.timestamp,
                &format!("dependency {phase_id} failed"),
            );
        }
        ReducerTransition::PhaseSkipped { reason } => {
            skip_phase(state, input, reason)?;
            let phase_id = required_phase_id(input)?;
            skip_pending_dependents(
                state,
                phase_id,
                &input.timestamp,
                &format!("dependency {phase_id} skipped"),
            );
        }
        ReducerTransition::PhaseRetrying => retry_phase(state, input)?,
        ReducerTransition::Unknown { .. } => {}
    }
    Ok(Vec::new())
}

fn complete_mission(state: &mut MissionState, timestamp: &str) -> Result<(), TransitionError> {
    require_in_progress(state)?;
    if state
        .phases
        .values()
        .any(|phase| phase.status == PhaseStatus::Failed)
    {
        return Err(TransitionError::MissionHasFailedPhase);
    }
    if state
        .phases
        .values()
        .any(|phase| !matches!(phase.status, PhaseStatus::Completed | PhaseStatus::Skipped))
    {
        return Err(TransitionError::MissionPhasesIncomplete);
    }
    state.status = MissionStatus::Completed;
    state.finished_at = Some(timestamp.to_owned());
    Ok(())
}

fn start_phase(state: &mut MissionState, input: &ReducerInput) -> Result<(), TransitionError> {
    require_in_progress(state)?;
    let phase_id = required_phase_id(input)?;
    let phase = state
        .phases
        .get(phase_id)
        .ok_or_else(|| TransitionError::UnknownPhase {
            phase_id: phase_id.clone(),
        })?;
    match phase.status {
        PhaseStatus::Pending => {}
        PhaseStatus::Running => {
            return Err(TransitionError::PhaseAlreadyRunning {
                phase_id: phase_id.clone(),
            });
        }
        status => {
            return Err(TransitionError::PhaseTerminal {
                phase_id: phase_id.clone(),
                status,
            });
        }
    }
    for dependency in &phase.dependencies {
        let dependency_status = state
            .phases
            .get(dependency)
            .map(|dependency| dependency.status)
            .ok_or_else(|| TransitionError::UnknownPhase {
                phase_id: dependency.clone(),
            })?;
        match dependency_status {
            PhaseStatus::Completed => {}
            PhaseStatus::Failed | PhaseStatus::Skipped => {
                return Err(TransitionError::DependencyFailed {
                    phase_id: phase_id.clone(),
                    dependency: dependency.clone(),
                    status: dependency_status,
                });
            }
            PhaseStatus::Pending | PhaseStatus::Running => {
                return Err(TransitionError::DependencyNotReady {
                    phase_id: phase_id.clone(),
                    dependency: dependency.clone(),
                });
            }
        }
    }
    let phase = state
        .phases
        .get_mut(phase_id)
        .ok_or_else(|| TransitionError::UnknownPhase {
            phase_id: phase_id.clone(),
        })?;
    phase.status = PhaseStatus::Running;
    phase.started_at = Some(input.timestamp.clone());
    Ok(())
}

fn complete_phase(
    state: &mut MissionState,
    input: &ReducerInput,
) -> Result<Vec<PhaseId>, TransitionError> {
    finish_phase(state, input, PhaseStatus::Completed, None, None)?;
    let completed = required_phase_id(input)?;
    let released = state
        .phase_order
        .iter()
        .filter(|candidate| {
            state.phases.get(*candidate).is_some_and(|phase| {
                phase.status == PhaseStatus::Pending
                    && phase.dependencies.contains(completed)
                    && phase.dependencies.iter().all(|dependency| {
                        state
                            .phases
                            .get(dependency)
                            .is_some_and(|dependency| dependency.status == PhaseStatus::Completed)
                    })
            })
        })
        .cloned()
        .collect();
    Ok(released)
}

fn finish_phase(
    state: &mut MissionState,
    input: &ReducerInput,
    status: PhaseStatus,
    error: Option<String>,
    skip_reason: Option<String>,
) -> Result<(), TransitionError> {
    require_in_progress(state)?;
    let phase_id = required_phase_id(input)?;
    let phase = state
        .phases
        .get_mut(phase_id)
        .ok_or_else(|| TransitionError::UnknownPhase {
            phase_id: phase_id.clone(),
        })?;
    match phase.status {
        PhaseStatus::Running => {}
        PhaseStatus::Pending => {
            return Err(TransitionError::PhaseNotRunning {
                phase_id: phase_id.clone(),
                status: phase.status,
            });
        }
        terminal => {
            return Err(TransitionError::PhaseTerminal {
                phase_id: phase_id.clone(),
                status: terminal,
            });
        }
    }
    phase.status = status;
    phase.finished_at = Some(input.timestamp.clone());
    phase.error = error;
    phase.skip_reason = skip_reason;
    Ok(())
}

fn skip_phase(
    state: &mut MissionState,
    input: &ReducerInput,
    reason: &str,
) -> Result<(), TransitionError> {
    let phase_id = required_phase_id(input)?;
    let phase = state
        .phases
        .get_mut(phase_id)
        .ok_or_else(|| TransitionError::UnknownPhase {
            phase_id: phase_id.clone(),
        })?;
    if phase.status.is_terminal() {
        return Err(TransitionError::PhaseTerminal {
            phase_id: phase_id.clone(),
            status: phase.status,
        });
    }
    phase.status = PhaseStatus::Skipped;
    phase.finished_at = Some(input.timestamp.clone());
    phase.skip_reason = Some(reason.to_owned());
    Ok(())
}

fn retry_phase(state: &mut MissionState, input: &ReducerInput) -> Result<(), TransitionError> {
    require_in_progress(state)?;
    let phase_id = required_phase_id(input)?;
    let phase = state
        .phases
        .get_mut(phase_id)
        .ok_or_else(|| TransitionError::UnknownPhase {
            phase_id: phase_id.clone(),
        })?;
    if phase.status != PhaseStatus::Running {
        return if phase.status.is_terminal() {
            Err(TransitionError::PhaseTerminal {
                phase_id: phase_id.clone(),
                status: phase.status,
            })
        } else {
            Err(TransitionError::PhaseNotRunning {
                phase_id: phase_id.clone(),
                status: phase.status,
            })
        };
    }
    phase.retry_observations = phase.retry_observations.checked_add(1).ok_or_else(|| {
        TransitionError::RetryCountOverflow {
            phase_id: phase_id.clone(),
        }
    })?;
    Ok(())
}

fn require_in_progress(state: &MissionState) -> Result<(), TransitionError> {
    if state.status == MissionStatus::InProgress {
        Ok(())
    } else {
        Err(TransitionError::MissionNotInProgress {
            status: state.status,
        })
    }
}

fn required_phase_id(input: &ReducerInput) -> Result<&PhaseId, TransitionError> {
    input
        .phase_id
        .as_ref()
        .ok_or(TransitionError::MissingPhaseId)
}

fn skip_all_active(state: &mut MissionState, timestamp: &str, reason: &str) {
    for phase in state.phases.values_mut() {
        if matches!(phase.status, PhaseStatus::Pending | PhaseStatus::Running) {
            phase.status = PhaseStatus::Skipped;
            phase.finished_at = Some(timestamp.to_owned());
            phase.skip_reason = Some(reason.to_owned());
        }
    }
}

fn skip_pending_dependents(
    state: &mut MissionState,
    root: &PhaseId,
    timestamp: &str,
    reason: &str,
) {
    let mut blocked = BTreeSet::from([root.clone()]);
    loop {
        let mut changed = false;
        for phase_id in &state.phase_order {
            let should_skip = state.phases.get(phase_id).is_some_and(|phase| {
                phase.status == PhaseStatus::Pending
                    && phase
                        .dependencies
                        .iter()
                        .any(|dependency| blocked.contains(dependency))
            });
            if should_skip {
                if let Some(phase) = state.phases.get_mut(phase_id) {
                    phase.status = PhaseStatus::Skipped;
                    phase.finished_at = Some(timestamp.to_owned());
                    phase.skip_reason = Some(reason.to_owned());
                }
                changed |= blocked.insert(phase_id.clone());
            }
        }
        if !changed {
            break;
        }
    }
}

pub(crate) fn detect_cycle(definitions: &[PhaseDefinition]) -> Result<(), MissionStateBuildError> {
    fn visit(
        phase_id: &PhaseId,
        dependencies: &BTreeMap<PhaseId, Vec<PhaseId>>,
        visiting: &mut BTreeSet<PhaseId>,
        visited: &mut BTreeSet<PhaseId>,
    ) -> Result<(), MissionStateBuildError> {
        if visited.contains(phase_id) {
            return Ok(());
        }
        if !visiting.insert(phase_id.clone()) {
            return Err(MissionStateBuildError::DependencyCycle {
                phase_id: phase_id.clone(),
            });
        }
        if let Some(edges) = dependencies.get(phase_id) {
            for dependency in edges {
                visit(dependency, dependencies, visiting, visited)?;
            }
        }
        visiting.remove(phase_id);
        visited.insert(phase_id.clone());
        Ok(())
    }

    let dependencies = definitions
        .iter()
        .map(|definition| (definition.id.clone(), definition.dependencies.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for phase_id in dependencies.keys() {
        visit(phase_id, &dependencies, &mut visiting, &mut visited)?;
    }
    Ok(())
}
