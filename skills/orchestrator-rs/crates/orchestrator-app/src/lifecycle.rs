//! Fixture-only lifecycle reduction and ordered event/checkpoint effects.

use crate::{
    FixtureProcessAuthority, FixtureProjectionWriter, OverlayError, OverlayExit, SettingsOverlay,
    WorkspaceAuthority, WorkspaceError,
    checkpoint_projection::{
        FIXTURE_EVENT_SEQUENCE_KEY, checkpoint_for_state, checkpoint_matches_state,
    },
    project_event,
    workspace::LifecycleProjectionFault,
};
use orchestrator_core::{
    CheckpointError, CheckpointProjection, CoreError, EventError, EventId, EventJsonMap,
    EventRecord, MissionState, MissionStatus, PhaseId, PhaseStatus, ReducerInput,
    ReducerTransition, TransitionError, VerificationDecision, decode_checkpoint, decode_event_line,
    encode_current_checkpoint, encode_current_event, reduce, scan_event_log,
};
use orchestrator_exec::{
    EventReceipt, EventSink, EventSinkError, EventSinkErrorKind, WorkerEventCodecError,
    WorkerEventDraft, WorkerEventEnvelope, WorkerEventError, WorkerEventKind,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use thiserror::Error;

/// Read-only facade for coordinator-owned process registry state.
pub trait OwnedChildState {
    /// True until every direct child is reaped, readers joined, and group ownership resolved.
    fn has_unresolved_children(&self) -> bool;
}

impl OwnedChildState for FixtureProcessAuthority {
    fn has_unresolved_children(&self) -> bool {
        self.has_unresolved_processes()
    }
}

/// Crate-wide serialization for tests that own real child processes.
///
/// The production implementations of [`OwnedChildState`] that back real
/// providers read the *process-wide* owned-process registry, which is a
/// deliberate safety property: a runtime must never release while any owned
/// child is still live. That makes the registry shared state across
/// concurrently running tests, so one test's live child makes another observe
/// `UnresolvedChildren` — or fail an "everything was reaped" assertion. Such
/// tests take a turn here instead of the production check being weakened to a
/// per-test view.
#[cfg(test)]
pub(crate) mod owned_process_serialization {
    use std::{
        cell::Cell,
        sync::{Mutex, MutexGuard, PoisonError},
    };

    static TURN: Mutex<()> = Mutex::new(());

    thread_local! {
        /// Nesting depth on this thread. [`Mutex`] is not reentrant and some
        /// tests legitimately build a second fixture to compare two
        /// authorities, so the outermost guard owns the lock and inner ones
        /// are no-ops.
        static DEPTH: Cell<usize> = const { Cell::new(0) };
    }

    /// Held for as long as a test may own a child process. Declare it last in
    /// a fixture struct so it is released after every other field.
    ///
    /// The guard is never read: holding it *is* the effect, and dropping it is
    /// what releases the turn.
    pub(crate) struct OwnedProcessTurn(
        #[expect(dead_code, reason = "held, not read")] Option<MutexGuard<'static, ()>>,
    );

    impl OwnedProcessTurn {
        /// Takes this thread's turn, or a no-op guard when already held.
        pub(crate) fn take() -> Self {
            let depth = DEPTH.with(|depth| {
                let current = depth.get();
                depth.set(current.saturating_add(1));
                current
            });
            Self((depth == 0).then(|| TURN.lock().unwrap_or_else(PoisonError::into_inner)))
        }
    }

    impl Drop for OwnedProcessTurn {
        fn drop(&mut self) {
            DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }
}

/// A durable transition acknowledgement. Replays perform no projection effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LifecycleAck {
    pub replayed: bool,
    pub mission_status: MissionStatus,
}

/// Terminal worker record returned when an exact attempt was already durable.
#[derive(Clone, Debug)]
pub struct DurableAttemptOutcome {
    attempt: u32,
    terminal: WorkerEventEnvelope,
}

impl DurableAttemptOutcome {
    /// Returns the non-zero attempt ordinal bound to every record in the stream.
    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Returns the durable terminal record, including its original receipt.
    #[must_use]
    pub const fn terminal(&self) -> &WorkerEventEnvelope {
        &self.terminal
    }
}

/// Distinguishes a newly executed closure from an already-durable attempt.
#[derive(Debug)]
pub enum FixtureAttemptRun<T> {
    Executed(T),
    Replayed(Box<DurableAttemptOutcome>),
}

impl<T> FixtureAttemptRun<T> {
    /// Returns the closure result only when new execution was admitted.
    #[must_use]
    pub fn into_executed(self) -> Option<T> {
        match self {
            Self::Executed(value) => Some(value),
            Self::Replayed(_) => None,
        }
    }

    /// Returns the durable terminal outcome when execution was short-circuited.
    #[must_use]
    pub const fn replayed(&self) -> Option<&DurableAttemptOutcome> {
        match self {
            Self::Executed(_) => None,
            Self::Replayed(outcome) => Some(outcome),
        }
    }
}

/// One-shot fixture fault boundaries for deterministic event/checkpoint ordering proofs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixtureProjectionFault {
    AfterEventSync,
    AfterCheckpointRename,
    AfterCheckpointRenameConflict,
    AfterCheckpointRenameIdentityChange,
}

impl FixtureProjectionFault {
    fn into_workspace(self) -> LifecycleProjectionFault {
        match self {
            FixtureProjectionFault::AfterEventSync => LifecycleProjectionFault::EventSynced,
            FixtureProjectionFault::AfterCheckpointRename => {
                LifecycleProjectionFault::CheckpointRenamed
            }
            FixtureProjectionFault::AfterCheckpointRenameConflict => {
                LifecycleProjectionFault::CheckpointRenamedWithConflict
            }
            FixtureProjectionFault::AfterCheckpointRenameIdentityChange => {
                LifecycleProjectionFault::CheckpointRenamedWithIdentityChange
            }
        }
    }
}

/// Fail-closed lifecycle boundary errors.
#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("lifecycle coordinator workspace, reducer, and checkpoint missions do not match")]
    MissionMismatch,
    #[error("checkpoint plan does not exactly cover reducer phases")]
    CheckpointPlanMismatch,
    #[error("lifecycle coordinator requires a pristine initial reducer state")]
    InitialStateNotPristine,
    #[error(
        "fixture checkpoint and event log do not describe an acknowledged or one-event-pending state"
    )]
    RecoveryConflict,
    #[error("the final durable event must be replayed to repair its checkpoint before new work")]
    RecoveryPending,
    #[error("fixture lifecycle coordinator does not implement retry or arbitrary event effects")]
    UnsupportedTransition,
    #[error("terminal lifecycle effect is blocked by unresolved owned children")]
    UnresolvedChildren,
    #[error("terminal lifecycle effect is blocked until the settings overlay is restored")]
    OverlayUnresolved,
    #[error("a settings overlay is already attached")]
    OverlayAlreadyAttached,
    #[error("phase completion requires a structured PASS verification gate")]
    VerificationNotPassed,
    #[error(transparent)]
    Reducer(#[from] TransitionError),
    #[error(transparent)]
    EventProjection(#[from] CoreError),
    #[error("cannot encode lifecycle event: {0}")]
    EventEncoding(#[from] EventError),
    #[error("cannot encode lifecycle checkpoint: {0}")]
    CheckpointEncoding(#[from] CheckpointError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    Overlay(#[from] OverlayError),
    #[error("lifecycle event data must be an object or null")]
    InvalidEventData,
    #[error("terminal lifecycle state has closed process admission")]
    AdmissionClosed,
    #[error("fixture worker events require a running bound phase")]
    AttemptPhaseNotRunning,
    #[error("fixture attempt worker identity is empty or mismatched")]
    AttemptWorkerMismatch,
    #[error("fixture attempt ordinal must be positive")]
    InvalidAttempt,
    #[error("an active durable attempt requires an explicit continuation protocol")]
    AttemptRecoveryRequired,
    #[error("fixture attempt returned without one durable terminal worker event")]
    AttemptTerminalMissing,
    #[error("fixture attempt ordinal cannot move behind durable worker history")]
    AttemptOrdinalRegression,
    #[error("successful lifecycle completion is blocked by an active worker attempt")]
    ActiveWorkerAttempt,
    #[error("legacy worker history has no attempt ordinal and cannot be resumed safely")]
    AmbiguousLegacyAttempt,
    #[error("fixture event sequence does not match the coordinator allocator")]
    EventSequenceMismatch,
    #[error("fixture event sequence allocator is exhausted")]
    EventSequenceExhausted,
    #[error(transparent)]
    WorkerEvent(#[from] WorkerEventError),
    #[error(transparent)]
    WorkerEventCodec(#[from] WorkerEventCodecError),
}

struct FixtureEventStampAllocator {
    next_sequence: Option<i64>,
}

impl FixtureEventStampAllocator {
    fn after(greatest_sequence: Option<i64>) -> Result<Self, LifecycleError> {
        let next_sequence = match greatest_sequence {
            Some(sequence) if sequence <= 0 => return Err(LifecycleError::RecoveryConflict),
            Some(sequence) => sequence.checked_add(1),
            None => Some(1),
        };
        if next_sequence.is_none() {
            return Err(LifecycleError::EventSequenceExhausted);
        }
        Ok(Self { next_sequence })
    }

    fn receipt(&self) -> Result<EventReceipt, LifecycleError> {
        let sequence = self
            .next_sequence
            .ok_or(LifecycleError::EventSequenceExhausted)?;
        EventReceipt::new(
            format!("evt_fixture_{sequence:019}"),
            format!("2000-01-01T00:00:00.{sequence:019}Z"),
            sequence,
        )
        .map_err(LifecycleError::from)
    }

    fn accepts(&self, sequence: i64) -> bool {
        self.next_sequence == Some(sequence)
    }

    fn advance(&mut self) {
        self.next_sequence = self
            .next_sequence
            .and_then(|sequence| sequence.checked_add(1));
    }
}

struct FixtureEventSinkErrors {
    rejected: EventSinkError,
    persistence: EventSinkError,
    indeterminate: EventSinkError,
    unavailable: EventSinkError,
}

impl FixtureEventSinkErrors {
    fn new() -> Result<Self, LifecycleError> {
        Ok(Self {
            rejected: EventSinkError::new(
                EventSinkErrorKind::Rejected,
                "fixture worker event is outside its running bound phase",
            )?,
            persistence: EventSinkError::new(
                EventSinkErrorKind::Persistence,
                "fixture worker event was not durably acknowledged",
            )?,
            indeterminate: EventSinkError::new(
                EventSinkErrorKind::Indeterminate,
                "fixture worker event requires authority recovery before continuation",
            )?,
            unavailable: EventSinkError::new(
                EventSinkErrorKind::Unavailable,
                "fixture worker event allocator is unavailable",
            )?,
        })
    }
}

/// Attempt-scoped fixture worker-event sink borrowed from a lifecycle coordinator.
///
/// It cannot be constructed independently or outlive the coordinator closure
/// that lent it. Every successful emission uses the coordinator's sole
/// projection writer and publishes in-memory state only after durable commit.
pub struct FixtureWorkerEventSink<'a> {
    writer: &'a mut FixtureProjectionWriter,
    state: &'a mut MissionState,
    checkpoint_template: &'a mut CheckpointProjection,
    checkpoint_bytes: &'a mut Vec<u8>,
    event_log_bytes: &'a mut Vec<u8>,
    acknowledged_events: &'a mut BTreeSet<EventId>,
    stamps: &'a mut FixtureEventStampAllocator,
    pending_recovery: &'a mut Option<PendingRecovery>,
    projection_fault: &'a mut Option<FixtureProjectionFault>,
    worker_streams: &'a mut BTreeMap<WorkerStreamKey, WorkerStream>,
    bound_phase: PhaseId,
    bound_worker: String,
    bound_attempt: u32,
    active_continuation: Option<ActiveContinuationState>,
    errors: FixtureEventSinkErrors,
}

impl fmt::Debug for FixtureWorkerEventSink<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureWorkerEventSink")
            .field("kind", &"attempt-scoped-fixture-worker-event-sink")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveContinuationState {
    AwaitExactSpawn,
    AwaitTerminal,
    Terminal,
    Faulted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveContinuationAction {
    ReplayExactSpawn,
    ProjectTerminal,
}

fn advance_active_continuation(
    state: &mut ActiveContinuationState,
    kind: WorkerEventKind,
) -> Result<ActiveContinuationAction, ()> {
    let (next, action) = match (*state, kind) {
        (ActiveContinuationState::AwaitExactSpawn, WorkerEventKind::Spawned) => (
            ActiveContinuationState::AwaitTerminal,
            ActiveContinuationAction::ReplayExactSpawn,
        ),
        (
            ActiveContinuationState::AwaitTerminal,
            WorkerEventKind::Completed | WorkerEventKind::Failed,
        ) => (
            ActiveContinuationState::Terminal,
            ActiveContinuationAction::ProjectTerminal,
        ),
        _ => {
            *state = ActiveContinuationState::Faulted;
            return Err(());
        }
    };
    *state = next;
    Ok(action)
}

fn next_worker_stream_state(
    current: Option<WorkerStreamState>,
    kind: WorkerEventKind,
) -> Option<WorkerStreamState> {
    match (current, kind) {
        (None, WorkerEventKind::Spawned) => Some(WorkerStreamState::Active),
        (Some(WorkerStreamState::Active), WorkerEventKind::Output) => {
            Some(WorkerStreamState::Active)
        }
        (Some(WorkerStreamState::Active), WorkerEventKind::Completed | WorkerEventKind::Failed) => {
            Some(WorkerStreamState::Terminal)
        }
        _ => None,
    }
}

impl FixtureWorkerEventSink<'_> {
    fn emit_inner(&mut self, event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError> {
        if self.pending_recovery.is_some() {
            return Err(self.errors.indeterminate.clone());
        }
        let identity = event.identity();
        let phase_is_running = self
            .state
            .phase(&self.bound_phase)
            .is_some_and(|phase| phase.status == PhaseStatus::Running);
        if !phase_is_running
            || identity.mission_id() != self.state.mission_id().as_str()
            || identity.phase_id() != self.bound_phase.as_str()
            || identity.worker_id() != self.bound_worker
            || event.attempt() != self.bound_attempt
        {
            return Err(self.errors.rejected.clone());
        }
        let worker_key = (
            self.bound_phase.as_str().to_owned(),
            self.bound_worker.clone(),
            Some(self.bound_attempt),
        );
        let current_worker_state = self
            .worker_streams
            .get(&worker_key)
            .map(|stream| stream.state);
        let rejected = self.errors.rejected.clone();
        let continuation_action = self
            .active_continuation
            .as_mut()
            .map(|state| advance_active_continuation(state, event.kind()).map_err(|()| rejected))
            .transpose()?;
        if continuation_action == Some(ActiveContinuationAction::ReplayExactSpawn) {
            if current_worker_state != Some(WorkerStreamState::Active) {
                return Err(self.errors.rejected.clone());
            }
            let Some(existing) = self.worker_streams.get(&worker_key).and_then(|stream| {
                (stream.events.len() == 1)
                    .then(|| stream.events.first())
                    .flatten()
            }) else {
                return Err(self.errors.rejected.clone());
            };
            if existing.envelope.kind() != WorkerEventKind::Spawned {
                return Err(self.errors.rejected.clone());
            }
            let candidate = event
                .into_envelope(existing.envelope.receipt().clone(), self.bound_attempt)
                .map_err(|_| self.errors.unavailable.clone())?;
            let candidate_json = candidate
                .to_json()
                .map_err(|_| self.errors.unavailable.clone())?;
            let existing_json = existing
                .envelope
                .to_json()
                .map_err(|_| self.errors.unavailable.clone())?;
            if candidate_json != existing_json {
                return Err(self.errors.rejected.clone());
            }
            let receipt = existing.envelope.receipt().clone();
            return Ok(receipt);
        }
        if self.active_continuation.is_some()
            && continuation_action != Some(ActiveContinuationAction::ProjectTerminal)
        {
            return Err(self.errors.rejected.clone());
        }
        let next_worker_state = next_worker_stream_state(current_worker_state, event.kind())
            .ok_or_else(|| self.errors.rejected.clone())?;

        let receipt = self
            .stamps
            .receipt()
            .map_err(|_| self.errors.unavailable.clone())?;
        let envelope = event
            .into_envelope(receipt.clone(), self.bound_attempt)
            .map_err(|_| self.errors.unavailable.clone())?;
        let encoded = envelope
            .to_json()
            .map_err(|_| self.errors.unavailable.clone())?;
        let decoded =
            decode_event_line(encoded.as_bytes()).map_err(|_| self.errors.unavailable.clone())?;
        let input = project_event(&decoded).map_err(|_| self.errors.unavailable.clone())?;
        let reduction = reduce(self.state, &input).map_err(|_| self.errors.rejected.clone())?;
        let mut checkpoint = checkpoint_for_state(self.checkpoint_template, &reduction.state)
            .map_err(|_| self.errors.unavailable.clone())?;
        checkpoint.extra.insert(
            FIXTURE_EVENT_SEQUENCE_KEY.to_owned(),
            Value::from(input.sequence),
        );
        let checkpoint_bytes =
            encode_current_checkpoint(&checkpoint).map_err(|_| self.errors.unavailable.clone())?;

        let fault = self
            .projection_fault
            .take()
            .map(FixtureProjectionFault::into_workspace);
        if let Err(error) = self.writer.commit_event_checkpoint_with_fault(
            encoded.as_bytes(),
            &checkpoint_bytes,
            fault,
        ) {
            if projection_error_is_indeterminate(&error) {
                *self.pending_recovery = Some(PendingRecovery {
                    input,
                    event_without_newline: encoded.as_bytes().to_vec(),
                    stamp_already_reserved: false,
                });
                return Err(self.errors.indeterminate.clone());
            }
            return Err(self.errors.persistence.clone());
        }
        self.event_log_bytes.extend_from_slice(encoded.as_bytes());
        self.event_log_bytes.push(b'\n');
        self.acknowledged_events.insert(input.event_id.clone());
        *self.checkpoint_template = checkpoint;
        *self.checkpoint_bytes = checkpoint_bytes;
        *self.state = reduction.state;
        let stream = self
            .worker_streams
            .entry(worker_key)
            .or_insert(WorkerStream {
                state: next_worker_state,
                events: Vec::new(),
            });
        stream.state = next_worker_state;
        stream.events.push(StoredWorkerEvent { envelope });
        self.stamps.advance();
        Ok(receipt)
    }
}

impl EventSink for FixtureWorkerEventSink<'_> {
    fn emit(&mut self, event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError> {
        let result = self.emit_inner(event);
        if result.is_err() {
            if let Some(state) = self.active_continuation.as_mut() {
                *state = ActiveContinuationState::Faulted;
            }
        }
        result
    }
}

#[derive(Clone)]
struct PendingRecovery {
    input: ReducerInput,
    event_without_newline: Vec<u8>,
    stamp_already_reserved: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum WorkerStreamState {
    Active,
    Terminal,
}

struct WorkerStream {
    state: WorkerStreamState,
    events: Vec<StoredWorkerEvent>,
}

struct StoredWorkerEvent {
    envelope: WorkerEventEnvelope,
}

type WorkerStreamKey = (String, String, Option<u32>);

/// Single-workspace fixture coordinator. Its workspace capability has no production constructor.
pub struct LifecycleCoordinator<R: OwnedChildState> {
    writer: FixtureProjectionWriter,
    registry: R,
    state: MissionState,
    checkpoint_template: CheckpointProjection,
    checkpoint_bytes: Vec<u8>,
    event_log_bytes: Vec<u8>,
    acknowledged_events: BTreeSet<EventId>,
    worker_streams: BTreeMap<WorkerStreamKey, WorkerStream>,
    event_stamps: FixtureEventStampAllocator,
    pending_recovery: Option<PendingRecovery>,
    overlay: Option<SettingsOverlay>,
    projection_fault: Option<FixtureProjectionFault>,
}

impl<R: OwnedChildState> LifecycleCoordinator<R> {
    /// Consumes the only workspace projection authority used by this coordinator.
    ///
    /// A checkpoint marker proves the exact reduced ordering state for Rust
    /// worker events. An exact fixture-authored trailing worker event without
    /// that marker is treated as a recoverable event-before-checkpoint crash.
    /// A trailing legacy Go worker event without a later lifecycle transition
    /// is ambiguous and therefore rejected instead of guessed acknowledged or
    /// pending; a later lifecycle transition may prove it was incorporated.
    pub fn new(
        workspace: WorkspaceAuthority,
        registry: R,
        initial_state: MissionState,
    ) -> Result<Self, LifecycleError> {
        if initial_state.status() != MissionStatus::NotStarted
            || initial_state.greatest_applied_sequence().is_some()
            || initial_state
                .phases()
                .any(|phase| phase.status != PhaseStatus::Pending)
        {
            return Err(LifecycleError::InitialStateNotPristine);
        }
        let writer = workspace.into_fixture_projection_writer()?;
        let checkpoint_bytes = writer.lifecycle_checkpoint_bytes().to_vec();
        let checkpoint_template = decode_checkpoint(&checkpoint_bytes)?.projection;
        if writer.mission_id() != initial_state.mission_id()
            || checkpoint_template.workspace_id != initial_state.mission_id().as_str()
        {
            return Err(LifecycleError::MissionMismatch);
        }
        let state_phases = initial_state
            .phases()
            .map(|phase| phase.id.as_str())
            .collect::<BTreeSet<_>>();
        let checkpoint_phases = checkpoint_template
            .plan
            .as_ref()
            .map(|plan| {
                plan.phases
                    .iter()
                    .map(|phase| phase.id.as_str())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        if state_phases != checkpoint_phases
            || checkpoint_template
                .plan
                .as_ref()
                .is_none_or(|plan| plan.phases.len() != state_phases.len())
        {
            return Err(LifecycleError::CheckpointPlanMismatch);
        }

        let event_log = writer.lifecycle_event_log_bytes().to_vec();
        if !event_log.is_empty() && !event_log.ends_with(b"\n") {
            return Err(LifecycleError::RecoveryConflict);
        }
        let scan = scan_event_log(&event_log);
        if !scan.diagnostics.is_empty() {
            return Err(LifecycleError::RecoveryConflict);
        }
        let mut reconstructed_states = vec![initial_state];
        let mut applied = Vec::with_capacity(scan.events.len());
        let mut checkpoint_match =
            checkpoint_matches_state(&checkpoint_template, &reconstructed_states[0])
                .then_some(0usize);
        for (index, event) in scan.events.iter().enumerate() {
            let input = project_event(event)?;
            let expected_sequence = i64::try_from(index)
                .ok()
                .and_then(|index| index.checked_add(1))
                .ok_or(LifecycleError::RecoveryConflict)?;
            if input.sequence != expected_sequence {
                return Err(LifecycleError::RecoveryConflict);
            }
            let current = reconstructed_states
                .last()
                .ok_or(LifecycleError::RecoveryConflict)?;
            validate_recovered_worker_boundary(current, &input)?;
            let next = reduce(current, &input)?.state;
            applied.push(input);
            let applied_input = applied.last().ok_or(LifecycleError::RecoveryConflict)?;
            // A worker observation changes reducer ordering but not the legacy
            // status/phase projection. Without the Rust sequence marker, it
            // must not falsely make an old checkpoint look acknowledged. A
            // later lifecycle transition can prove a legacy Go worker record
            // was incorporated; an ambiguous trailing legacy worker fails
            // closed below. Fixture-authored trailing workers are recoverable
            // because their ID/timestamp source is exact and deterministic.
            let unmarked_worker = !checkpoint_template
                .extra
                .contains_key(FIXTURE_EVENT_SEQUENCE_KEY)
                && is_worker_event(applied_input);
            if !unmarked_worker && checkpoint_matches_state(&checkpoint_template, &next) {
                checkpoint_match = Some(index + 1);
            }
            reconstructed_states.push(next);
        }
        let matched = checkpoint_match.ok_or(LifecycleError::RecoveryConflict)?;
        if applied.len().saturating_sub(matched) > 1 {
            return Err(LifecycleError::RecoveryConflict);
        }
        let greatest_event_sequence = applied.iter().map(|input| input.sequence).max();
        let event_stamps = FixtureEventStampAllocator::after(greatest_event_sequence)?;
        let mut worker_streams = BTreeMap::new();
        for (input, event) in applied.iter().zip(&scan.events) {
            recover_worker_stream(&mut worker_streams, input, &event.raw_line)?;
        }
        let acknowledged_events = applied[..matched]
            .iter()
            .map(|input| input.event_id.clone())
            .collect();
        let pending_recovery = applied.get(matched).cloned().map(|input| PendingRecovery {
            input,
            event_without_newline: scan.events[matched]
                .raw_line
                .strip_suffix(b"\n")
                .unwrap_or(&scan.events[matched].raw_line)
                .to_vec(),
            stamp_already_reserved: true,
        });
        if pending_recovery.as_ref().is_some_and(|pending| {
            is_worker_event(&pending.input)
                && !checkpoint_template
                    .extra
                    .contains_key(FIXTURE_EVENT_SEQUENCE_KEY)
                && !is_fixture_authored_worker_event(&pending.input)
        }) {
            return Err(LifecycleError::RecoveryConflict);
        }
        let state = reconstructed_states
            .get(matched)
            .cloned()
            .ok_or(LifecycleError::RecoveryConflict)?;
        writer.confirm_lifecycle_projection()?;
        let mut coordinator = Self {
            writer,
            registry,
            state,
            checkpoint_template,
            checkpoint_bytes,
            event_log_bytes: event_log,
            acknowledged_events,
            worker_streams,
            event_stamps,
            pending_recovery,
            overlay: None,
            projection_fault: None,
        };
        if coordinator
            .pending_recovery
            .as_ref()
            .is_some_and(|pending| is_worker_event(&pending.input))
        {
            coordinator.repair_pending_worker_event()?;
        }
        Ok(coordinator)
    }

    /// Returns the acknowledged reducer state.
    #[must_use]
    pub const fn state(&self) -> &MissionState {
        &self.state
    }

    /// Returns the registry facade so fixture callers may spawn owned work through it.
    pub fn registry(&self) -> Result<&R, LifecycleError> {
        if self.pending_recovery.is_some() {
            return Err(LifecycleError::RecoveryPending);
        }
        if self.state.status().is_terminal() {
            Err(LifecycleError::AdmissionClosed)
        } else {
            Ok(&self.registry)
        }
    }

    /// Repairs only the exact pending mission/phase start transition retained
    /// after an indeterminate fixture projection result.
    ///
    /// Worker records and terminal lifecycle effects require their own
    /// recovery protocols and remain fail-closed. Returning `false` means no
    /// transition was pending; returning `true` means the retained transition
    /// is now durably acknowledged.
    pub fn repair_pending_admission_transition(&mut self) -> Result<bool, LifecycleError> {
        let Some(pending) = self.pending_recovery.as_ref() else {
            return Ok(false);
        };
        if !matches!(
            pending.input.transition,
            ReducerTransition::MissionStarted | ReducerTransition::PhaseStarted
        ) {
            return Err(LifecycleError::RecoveryPending);
        }
        self.repair_pending_lifecycle_transition(None)
    }

    /// Repairs the exact pending non-worker lifecycle transition retained
    /// after an indeterminate fixture projection result.
    ///
    /// The caller must supply the same structured verification decision when
    /// repairing `phase.completed`; the normal terminal child/overlay
    /// boundaries are re-evaluated before acknowledgement. Worker events keep
    /// their separate recovery protocol and cannot be repaired through this
    /// method.
    pub(crate) fn repair_pending_lifecycle_transition(
        &mut self,
        verification: Option<VerificationDecision>,
    ) -> Result<bool, LifecycleError> {
        let Some(pending) = self.pending_recovery.as_ref() else {
            return Ok(false);
        };
        if is_worker_event(&pending.input) {
            return Err(LifecycleError::RecoveryPending);
        }
        let input = pending.input.clone();
        let acknowledgement = self.transition(&input, verification)?;
        Ok(acknowledgement.replayed)
    }

    /// Returns the exact pending non-worker lifecycle transition, if one must
    /// be repaired before new work can proceed.
    ///
    /// This read-only view lets a caller compare a retained terminal
    /// transition with its independently persisted decision before
    /// [`Self::repair_pending_lifecycle_transition`] acknowledges anything.
    /// Worker-event recovery remains private to the coordinator.
    pub(crate) fn pending_lifecycle_transition(&self) -> Option<&ReducerTransition> {
        self.pending_recovery.as_ref().and_then(|pending| {
            (!is_worker_event(&pending.input)).then_some(&pending.input.transition)
        })
    }

    /// Returns an exact durable terminal attempt without opening new work.
    ///
    /// Active, legacy-ambiguous, and regressing histories return the same
    /// fail-closed errors as [`Self::with_attempt`]. This read-only query is
    /// intentionally independent of registry admission so replay remains
    /// available after mission closure.
    pub fn durable_attempt_replay(
        &self,
        phase: &PhaseId,
        worker_id: &str,
        attempt: u32,
    ) -> Result<Option<DurableAttemptOutcome>, LifecycleError> {
        if self.pending_recovery.is_some() {
            return Err(LifecycleError::RecoveryPending);
        }
        if worker_id.is_empty() {
            return Err(LifecycleError::AttemptWorkerMismatch);
        }
        if attempt == 0 {
            return Err(LifecycleError::InvalidAttempt);
        }
        let phase_name = phase.as_str().to_owned();
        let worker_name = worker_id.to_owned();
        if self
            .worker_streams
            .contains_key(&(phase_name.clone(), worker_name.clone(), None))
        {
            return Err(LifecycleError::AmbiguousLegacyAttempt);
        }
        let worker_key = (phase_name, worker_name, Some(attempt));
        if let Some(stream) = self.worker_streams.get(&worker_key) {
            if stream.state == WorkerStreamState::Active {
                return Err(LifecycleError::AttemptRecoveryRequired);
            }
            let terminal = stream
                .events
                .last()
                .map(|stored| stored.envelope.clone())
                .ok_or(LifecycleError::RecoveryConflict)?;
            if terminal.attempt() != Some(attempt)
                || !matches!(
                    terminal.kind(),
                    WorkerEventKind::Completed | WorkerEventKind::Failed
                )
            {
                return Err(LifecycleError::RecoveryConflict);
            }
            return Ok(Some(DurableAttemptOutcome { attempt, terminal }));
        }
        let mut greatest_attempt = None;
        for ((stored_phase, stored_worker, stored_attempt), stream) in &self.worker_streams {
            if stored_phase != phase.as_str() || stored_worker != worker_id {
                continue;
            }
            if stream.state == WorkerStreamState::Active {
                return Err(LifecycleError::AttemptRecoveryRequired);
            }
            if let Some(stored_attempt) = stored_attempt {
                greatest_attempt =
                    Some(greatest_attempt.map_or(*stored_attempt, |greatest: u32| {
                        greatest.max(*stored_attempt)
                    }));
            }
        }
        if greatest_attempt.is_some_and(|greatest| attempt <= greatest) {
            return Err(LifecycleError::AttemptOrdinalRegression);
        }
        Ok(None)
    }

    /// Borrows the registry and the coordinator's sole worker-event sink for
    /// one attempt worker and non-zero ordinal bound to an already-running phase.
    ///
    /// The closure shape prevents the sink or projection writer from escaping
    /// the coordinator borrow. Worker identity is checked again on every
    /// emission, so a context for another mission, phase, worker, or attempt
    /// fails closed. A terminal exact attempt returns its durable outcome
    /// without invoking the closure, including after the enclosing phase and
    /// mission have completed. Active history is never replayed through an
    /// arbitrary executor because event deduplication cannot prove that earlier
    /// process or effect work is safe to repeat; it requires a future explicit
    /// continuation protocol instead. A higher attempt ordinal may start only
    /// after every earlier stream for that phase worker is terminal.
    pub fn with_attempt<T>(
        &mut self,
        phase: &PhaseId,
        worker_id: &str,
        attempt: u32,
        operation: impl FnOnce(&R, &mut FixtureWorkerEventSink<'_>) -> T,
    ) -> Result<FixtureAttemptRun<T>, LifecycleError> {
        if let Some(outcome) = self.durable_attempt_replay(phase, worker_id, attempt)? {
            return Ok(FixtureAttemptRun::Replayed(Box::new(outcome)));
        }
        let worker_key = (
            phase.as_str().to_owned(),
            worker_id.to_owned(),
            Some(attempt),
        );
        if self.state.status().is_terminal() {
            return Err(LifecycleError::AdmissionClosed);
        }
        if self
            .state
            .phase(phase)
            .is_none_or(|state| state.status != PhaseStatus::Running)
        {
            return Err(LifecycleError::AttemptPhaseNotRunning);
        }
        let errors = FixtureEventSinkErrors::new()?;
        let registry = &self.registry;
        let mut sink = FixtureWorkerEventSink {
            writer: &mut self.writer,
            state: &mut self.state,
            checkpoint_template: &mut self.checkpoint_template,
            checkpoint_bytes: &mut self.checkpoint_bytes,
            event_log_bytes: &mut self.event_log_bytes,
            acknowledged_events: &mut self.acknowledged_events,
            stamps: &mut self.event_stamps,
            pending_recovery: &mut self.pending_recovery,
            projection_fault: &mut self.projection_fault,
            worker_streams: &mut self.worker_streams,
            bound_phase: phase.clone(),
            bound_worker: worker_id.to_owned(),
            bound_attempt: attempt,
            active_continuation: None,
            errors,
        };
        let result = operation(registry, &mut sink);
        if self.pending_recovery.is_some() {
            return Err(LifecycleError::RecoveryPending);
        }
        if self
            .worker_streams
            .get(&worker_key)
            .is_none_or(|stream| stream.state != WorkerStreamState::Terminal)
        {
            return Err(LifecycleError::AttemptTerminalMissing);
        }
        Ok(FixtureAttemptRun::Executed(result))
    }

    /// Projects a precomputed terminal outcome into one exact active stream.
    ///
    /// This is deliberately narrower than [`Self::with_attempt`]: the durable
    /// stream must contain exactly one fixture-authored `worker.spawned`
    /// envelope for the same phase/worker/attempt, and the operation's first
    /// emission must reproduce that envelope byte for byte. The duplicate
    /// spawn is acknowledged with its original receipt without advancing the
    /// allocator or touching disk; only the terminal event is new.
    ///
    /// The caller remains responsible for proving that execution already
    /// reached a durable terminal decision. No ordinary executor admission
    /// path calls this method, and [`Self::with_attempt`] keeps rejecting every
    /// active stream.
    pub(crate) fn project_terminal_into_active_attempt<T>(
        &mut self,
        phase: &PhaseId,
        worker_id: &str,
        attempt: u32,
        operation: impl FnOnce(&R, &mut FixtureWorkerEventSink<'_>) -> T,
    ) -> Result<FixtureAttemptRun<T>, LifecycleError> {
        if self.pending_recovery.is_some() {
            return Err(LifecycleError::RecoveryPending);
        }
        if worker_id.is_empty() {
            return Err(LifecycleError::AttemptWorkerMismatch);
        }
        if attempt == 0 {
            return Err(LifecycleError::InvalidAttempt);
        }
        if self.worker_streams.contains_key(&(
            phase.as_str().to_owned(),
            worker_id.to_owned(),
            None,
        )) {
            return Err(LifecycleError::AmbiguousLegacyAttempt);
        }
        let worker_key = (
            phase.as_str().to_owned(),
            worker_id.to_owned(),
            Some(attempt),
        );
        for ((stored_phase, stored_worker, stored_attempt), stream) in &self.worker_streams {
            if stored_phase != phase.as_str() || stored_worker != worker_id {
                continue;
            }
            let Some(stored_attempt) = stored_attempt else {
                return Err(LifecycleError::AmbiguousLegacyAttempt);
            };
            if *stored_attempt > attempt {
                return Err(LifecycleError::AttemptOrdinalRegression);
            }
            if *stored_attempt != attempt && stream.state == WorkerStreamState::Active {
                return Err(LifecycleError::AttemptRecoveryRequired);
            }
        }
        let exact_spawn = self.worker_streams.get(&worker_key).is_some_and(|stream| {
            stream.state == WorkerStreamState::Active
                && stream.events.len() == 1
                && stream.events.first().is_some_and(|stored| {
                    stored.envelope.kind() == WorkerEventKind::Spawned
                        && is_fixture_authored_worker_envelope(&stored.envelope)
                })
        });
        if !exact_spawn {
            return Err(LifecycleError::AttemptRecoveryRequired);
        }
        if self.state.status().is_terminal() {
            return Err(LifecycleError::AdmissionClosed);
        }
        if self
            .state
            .phase(phase)
            .is_none_or(|state| state.status != PhaseStatus::Running)
        {
            return Err(LifecycleError::AttemptPhaseNotRunning);
        }
        let errors = FixtureEventSinkErrors::new()?;
        let registry = &self.registry;
        let mut sink = FixtureWorkerEventSink {
            writer: &mut self.writer,
            state: &mut self.state,
            checkpoint_template: &mut self.checkpoint_template,
            checkpoint_bytes: &mut self.checkpoint_bytes,
            event_log_bytes: &mut self.event_log_bytes,
            acknowledged_events: &mut self.acknowledged_events,
            stamps: &mut self.event_stamps,
            pending_recovery: &mut self.pending_recovery,
            projection_fault: &mut self.projection_fault,
            worker_streams: &mut self.worker_streams,
            bound_phase: phase.clone(),
            bound_worker: worker_id.to_owned(),
            bound_attempt: attempt,
            active_continuation: Some(ActiveContinuationState::AwaitExactSpawn),
            errors,
        };
        let result = operation(registry, &mut sink);
        let continuation_state = sink.active_continuation;
        drop(sink);
        if self.pending_recovery.is_some() {
            return Err(LifecycleError::RecoveryPending);
        }
        if continuation_state != Some(ActiveContinuationState::Terminal) {
            return Err(LifecycleError::AttemptTerminalMissing);
        }
        if self
            .worker_streams
            .get(&worker_key)
            .is_none_or(|stream| stream.state != WorkerStreamState::Terminal)
        {
            return Err(LifecycleError::AttemptTerminalMissing);
        }
        Ok(FixtureAttemptRun::Executed(result))
    }

    /// Transfers one installed fixture overlay into coordinator teardown ownership.
    pub fn attach_overlay(&mut self, overlay: SettingsOverlay) -> Result<(), LifecycleError> {
        if self.pending_recovery.is_some() {
            return Err(LifecycleError::RecoveryPending);
        }
        if self.state.status().is_terminal() {
            return Err(LifecycleError::AdmissionClosed);
        }
        if self.overlay.is_some() {
            return Err(LifecycleError::OverlayAlreadyAttached);
        }
        self.overlay = Some(overlay);
        Ok(())
    }

    /// Restores the attached overlay. Failure retains it and blocks every terminal effect.
    pub fn restore_overlay(&mut self, exit: OverlayExit) -> Result<(), LifecycleError> {
        let Some(overlay) = self.overlay.as_ref() else {
            return Ok(());
        };
        overlay.clone().finish(exit)?;
        let _restored = self
            .overlay
            .take()
            .ok_or(LifecycleError::OverlayUnresolved)?;
        Ok(())
    }

    /// Stops the next fixture projection once at a durable ordering boundary.
    pub fn inject_fixture_projection_fault_once(&mut self, fault: FixtureProjectionFault) {
        self.projection_fault = Some(fault);
    }

    /// Allocates fixture-only lifecycle metadata from the same sequence, ID,
    /// and RFC3339 source used by worker events, then commits the transition.
    pub fn transition_allocated(
        &mut self,
        phase_id: Option<PhaseId>,
        transition: ReducerTransition,
        data: Value,
        verification: Option<VerificationDecision>,
    ) -> Result<LifecycleAck, LifecycleError> {
        let receipt = self.event_stamps.receipt()?;
        let input = ReducerInput {
            event_id: EventId::new(receipt.id())?,
            sequence: receipt.sequence(),
            timestamp: receipt.timestamp().to_owned(),
            mission_id: self.state.mission_id().clone(),
            phase_id,
            worker_id: None,
            data,
            extra: BTreeMap::new(),
            transition,
        };
        self.transition(&input, verification)
    }

    /// Reduces, encodes, and durably projects one supported fixture lifecycle transition.
    /// In-memory state is published only after event and checkpoint are both durable.
    pub fn transition(
        &mut self,
        input: &ReducerInput,
        verification: Option<VerificationDecision>,
    ) -> Result<LifecycleAck, LifecycleError> {
        if let Some(pending) = self.pending_recovery.clone() {
            if pending.input != *input {
                return Err(LifecycleError::RecoveryPending);
            }
            // Worker events also carry attempt-stream authority that the generic
            // reducer transition cannot reconstruct safely. Reopening the
            // coordinator repairs both the durable projection and worker stream
            // atomically before admission becomes available again.
            if is_worker_event(&pending.input) {
                return Err(LifecycleError::RecoveryPending);
            }
            let reduction = reduce(&self.state, input)?;
            self.validate_effect_boundary(input, verification)?;
            let event_bytes = pending.event_without_newline;
            let stamp_already_reserved = pending.stamp_already_reserved;
            // checkpoint_for_state already guarantees FIXTURE_EVENT_SEQUENCE_KEY
            // is present only when self.checkpoint_template carried it, so no
            // extra cleanup is needed here.
            let checkpoint = checkpoint_for_state(&self.checkpoint_template, &reduction.state)?;
            let checkpoint_bytes = encode_current_checkpoint(&checkpoint)?;
            let fault = self
                .projection_fault
                .take()
                .map(FixtureProjectionFault::into_workspace);
            self.writer.commit_event_checkpoint_with_fault(
                &event_bytes,
                &checkpoint_bytes,
                fault,
            )?;
            let mut event_tail = event_bytes;
            event_tail.push(b'\n');
            if !self.event_log_bytes.ends_with(&event_tail) {
                self.event_log_bytes.extend_from_slice(&event_tail);
            }
            self.checkpoint_template = checkpoint;
            self.checkpoint_bytes = checkpoint_bytes;
            self.acknowledged_events.insert(input.event_id.clone());
            self.pending_recovery = None;
            self.state = reduction.state;
            if !stamp_already_reserved {
                self.event_stamps.advance();
            }
            return Ok(LifecycleAck {
                replayed: true,
                mission_status: self.state.status(),
            });
        }

        // Exact acknowledged replay must not touch process, overlay, or projection state.
        if self.state.applied_event(&input.event_id).is_some() {
            let _replay = reduce(&self.state, input)?;
            if !self.acknowledged_events.contains(&input.event_id) {
                return Err(LifecycleError::RecoveryConflict);
            }
            return Ok(LifecycleAck {
                replayed: true,
                mission_status: self.state.status(),
            });
        }

        if matches!(
            input.transition,
            ReducerTransition::PhaseRetrying | ReducerTransition::Unknown { .. }
        ) {
            return Err(LifecycleError::UnsupportedTransition);
        }
        if !self.event_stamps.accepts(input.sequence) {
            return Err(LifecycleError::EventSequenceMismatch);
        }
        self.validate_effect_boundary(input, verification)?;

        let reduction = reduce(&self.state, input)?;
        let event = event_record(input)?;
        let event_bytes = encode_current_event(&event)?;
        let checkpoint = checkpoint_for_state(&self.checkpoint_template, &reduction.state)?;
        let checkpoint_bytes = encode_current_checkpoint(&checkpoint)?;
        let fault = self
            .projection_fault
            .take()
            .map(FixtureProjectionFault::into_workspace);

        if let Err(error) =
            self.writer
                .commit_event_checkpoint_with_fault(&event_bytes, &checkpoint_bytes, fault)
        {
            if projection_error_is_indeterminate(&error) {
                self.pending_recovery = Some(PendingRecovery {
                    input: input.clone(),
                    event_without_newline: event_bytes,
                    stamp_already_reserved: false,
                });
            }
            return Err(error.into());
        }
        self.event_log_bytes.extend_from_slice(&event_bytes);
        self.event_log_bytes.push(b'\n');
        self.checkpoint_template = checkpoint;
        self.checkpoint_bytes = checkpoint_bytes;
        self.state = reduction.state;
        self.acknowledged_events.insert(input.event_id.clone());
        self.event_stamps.advance();
        Ok(LifecycleAck {
            replayed: false,
            mission_status: self.state.status(),
        })
    }

    fn repair_pending_worker_event(&mut self) -> Result<(), LifecycleError> {
        let pending = self
            .pending_recovery
            .clone()
            .ok_or(LifecycleError::RecoveryConflict)?;
        if !is_worker_event(&pending.input) {
            return Err(LifecycleError::RecoveryConflict);
        }
        let reduction = reduce(&self.state, &pending.input)?;
        let mut checkpoint = checkpoint_for_state(&self.checkpoint_template, &reduction.state)?;
        checkpoint.extra.insert(
            FIXTURE_EVENT_SEQUENCE_KEY.to_owned(),
            Value::from(pending.input.sequence),
        );
        let checkpoint_bytes = encode_current_checkpoint(&checkpoint)?;
        self.writer
            .commit_event_checkpoint(&pending.event_without_newline, &checkpoint_bytes)?;
        self.checkpoint_template = checkpoint;
        self.checkpoint_bytes = checkpoint_bytes;
        self.acknowledged_events
            .insert(pending.input.event_id.clone());
        self.pending_recovery = None;
        self.state = reduction.state;
        Ok(())
    }

    fn validate_effect_boundary(
        &self,
        input: &ReducerInput,
        verification: Option<VerificationDecision>,
    ) -> Result<(), LifecycleError> {
        if is_terminal_effect(&input.transition) {
            if self.registry.has_unresolved_children() {
                return Err(LifecycleError::UnresolvedChildren);
            }
            if self.overlay.is_some() {
                return Err(LifecycleError::OverlayUnresolved);
            }
        }
        if matches!(input.transition, ReducerTransition::PhaseCompleted)
            && !verification.is_some_and(VerificationDecision::gate_passed)
        {
            return Err(LifecycleError::VerificationNotPassed);
        }
        if matches!(input.transition, ReducerTransition::PhaseCompleted)
            && input.phase_id.as_ref().is_some_and(|phase| {
                self.worker_streams
                    .iter()
                    .any(|((stored_phase, _, _), stream)| {
                        stored_phase == phase.as_str() && stream.state == WorkerStreamState::Active
                    })
            })
        {
            return Err(LifecycleError::ActiveWorkerAttempt);
        }
        if matches!(input.transition, ReducerTransition::MissionCompleted)
            && self
                .worker_streams
                .values()
                .any(|stream| stream.state == WorkerStreamState::Active)
        {
            return Err(LifecycleError::ActiveWorkerAttempt);
        }
        Ok(())
    }
}

fn projection_error_is_indeterminate(error: &WorkspaceError) -> bool {
    matches!(
        error,
        WorkspaceError::InjectedProjectionFault(_)
            | WorkspaceError::RecoveryRequired { .. }
            | WorkspaceError::ProjectionIndeterminate { .. }
    )
}

fn worker_event_kind(input: &ReducerInput) -> Option<WorkerEventKind> {
    let ReducerTransition::Unknown { event_type, .. } = &input.transition else {
        return None;
    };
    match event_type.as_str() {
        "worker.spawned" => Some(WorkerEventKind::Spawned),
        "worker.output" => Some(WorkerEventKind::Output),
        "worker.completed" => Some(WorkerEventKind::Completed),
        "worker.failed" => Some(WorkerEventKind::Failed),
        _ => None,
    }
}

fn is_worker_event(input: &ReducerInput) -> bool {
    worker_event_kind(input).is_some()
        && input.phase_id.is_some()
        && input
            .worker_id
            .as_deref()
            .is_some_and(|worker| !worker.is_empty())
}

fn validate_recovered_worker_boundary(
    state: &MissionState,
    input: &ReducerInput,
) -> Result<(), LifecycleError> {
    if worker_event_kind(input).is_none() {
        return Ok(());
    }
    let phase = input
        .phase_id
        .as_ref()
        .and_then(|phase| state.phase(phase))
        .ok_or(LifecycleError::RecoveryConflict)?;
    if state.status() != MissionStatus::InProgress
        || phase.status != PhaseStatus::Running
        || input
            .worker_id
            .as_deref()
            .is_none_or(|worker| worker.is_empty())
    {
        return Err(LifecycleError::RecoveryConflict);
    }
    Ok(())
}

fn recover_worker_stream(
    streams: &mut BTreeMap<WorkerStreamKey, WorkerStream>,
    input: &ReducerInput,
    raw_line: &[u8],
) -> Result<(), LifecycleError> {
    let kind = worker_event_kind(input);
    let Some(kind) = kind else {
        return Ok(());
    };
    let phase = input
        .phase_id
        .as_ref()
        .ok_or(LifecycleError::RecoveryConflict)?;
    let worker = input
        .worker_id
        .as_ref()
        .filter(|worker| !worker.is_empty())
        .ok_or(LifecycleError::RecoveryConflict)?;
    let raw_line = raw_line.strip_suffix(b"\n").unwrap_or(raw_line);
    let json = std::str::from_utf8(raw_line).map_err(|_| LifecycleError::RecoveryConflict)?;
    let envelope =
        WorkerEventEnvelope::from_json(json).map_err(|_| LifecycleError::RecoveryConflict)?;
    if envelope.preserved_source_json() != Some(raw_line)
        || envelope.receipt().id() != input.event_id.as_str()
        || envelope.receipt().timestamp() != input.timestamp
        || envelope.receipt().sequence() != input.sequence
        || envelope.identity().mission_id() != input.mission_id.as_str()
        || envelope.identity().phase_id() != phase.as_str()
        || envelope.identity().worker_id() != worker
    {
        return Err(LifecycleError::RecoveryConflict);
    }
    let key = (
        phase.as_str().to_owned(),
        worker.clone(),
        envelope.attempt(),
    );
    let current = streams.get(&key).map(|stream| stream.state);
    let next = match (current, kind) {
        (None, WorkerEventKind::Spawned) => WorkerStreamState::Active,
        (Some(WorkerStreamState::Active), WorkerEventKind::Output) => WorkerStreamState::Active,
        (Some(WorkerStreamState::Active), WorkerEventKind::Completed | WorkerEventKind::Failed) => {
            WorkerStreamState::Terminal
        }
        _ => return Err(LifecycleError::RecoveryConflict),
    };
    let stream = streams.entry(key).or_insert(WorkerStream {
        state: next,
        events: Vec::new(),
    });
    stream.state = next;
    stream.events.push(StoredWorkerEvent { envelope });
    Ok(())
}

fn is_fixture_authored_worker_event(input: &ReducerInput) -> bool {
    input.sequence > 0
        && is_worker_event(input)
        && input.event_id.as_str() == format!("evt_fixture_{:019}", input.sequence)
        && input.timestamp == format!("2000-01-01T00:00:00.{:019}Z", input.sequence)
}

fn is_fixture_authored_worker_envelope(envelope: &WorkerEventEnvelope) -> bool {
    let receipt = envelope.receipt();
    receipt.sequence() > 0
        && receipt.id() == format!("evt_fixture_{:019}", receipt.sequence())
        && receipt.timestamp() == format!("2000-01-01T00:00:00.{:019}Z", receipt.sequence())
}

fn is_terminal_effect(transition: &ReducerTransition) -> bool {
    matches!(
        transition,
        ReducerTransition::MissionCompleted
            | ReducerTransition::MissionFailed
            | ReducerTransition::MissionCancelled { .. }
            | ReducerTransition::PhaseCompleted
            | ReducerTransition::PhaseFailed { .. }
            | ReducerTransition::PhaseSkipped { .. }
    )
}

fn event_record(input: &ReducerInput) -> Result<EventRecord, LifecycleError> {
    let event_type = match &input.transition {
        ReducerTransition::MissionStarted => "mission.started",
        ReducerTransition::MissionCompleted => "mission.completed",
        ReducerTransition::MissionFailed => "mission.failed",
        ReducerTransition::MissionCancelled { .. } => "mission.cancelled",
        ReducerTransition::PhaseStarted => "phase.started",
        ReducerTransition::PhaseCompleted => "phase.completed",
        ReducerTransition::PhaseFailed { .. } => "phase.failed",
        ReducerTransition::PhaseSkipped { .. } => "phase.skipped",
        ReducerTransition::PhaseRetrying | ReducerTransition::Unknown { .. } => {
            return Err(LifecycleError::UnsupportedTransition);
        }
    };
    let data = match &input.data {
        Value::Null => None,
        Value::Object(values) => Some(
            values
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<EventJsonMap>(),
        ),
        _ => return Err(LifecycleError::InvalidEventData),
    };
    Ok(EventRecord {
        id: input.event_id.to_string(),
        event_type: event_type.to_owned(),
        timestamp: input.timestamp.clone(),
        sequence: input.sequence,
        mission_id: input.mission_id.to_string(),
        phase_id: input.phase_id.as_ref().map(ToString::to_string),
        worker_id: input.worker_id.clone(),
        data,
        extra: EventJsonMap::from(input.extra.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_continuation_state_machine_is_spawn_then_terminal_only() {
        for kind in [
            WorkerEventKind::Output,
            WorkerEventKind::Completed,
            WorkerEventKind::Failed,
        ] {
            let mut state = ActiveContinuationState::AwaitExactSpawn;
            assert!(advance_active_continuation(&mut state, kind).is_err());
            assert_eq!(state, ActiveContinuationState::Faulted);
        }

        let mut state = ActiveContinuationState::AwaitExactSpawn;
        assert_eq!(
            advance_active_continuation(&mut state, WorkerEventKind::Spawned),
            Ok(ActiveContinuationAction::ReplayExactSpawn)
        );
        assert_eq!(state, ActiveContinuationState::AwaitTerminal);
        for kind in [WorkerEventKind::Spawned, WorkerEventKind::Output] {
            let mut invalid = state;
            assert!(advance_active_continuation(&mut invalid, kind).is_err());
            assert_eq!(invalid, ActiveContinuationState::Faulted);
            assert!(advance_active_continuation(&mut invalid, WorkerEventKind::Completed).is_err());
            assert_eq!(invalid, ActiveContinuationState::Faulted);
        }
        for kind in [WorkerEventKind::Completed, WorkerEventKind::Failed] {
            let mut terminal = state;
            assert_eq!(
                advance_active_continuation(&mut terminal, kind),
                Ok(ActiveContinuationAction::ProjectTerminal)
            );
            assert_eq!(terminal, ActiveContinuationState::Terminal);
            assert!(advance_active_continuation(&mut terminal, WorkerEventKind::Failed).is_err());
            assert_eq!(terminal, ActiveContinuationState::Faulted);
        }
    }
}
