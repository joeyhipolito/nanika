//! Cell 2D: journal-first hermetic compatibility projector.
//!
//! `HermeticCompatibilityProjector` is the only composition in this crate
//! that simultaneously holds [`RuntimeStore`] writer authority, production
//! workspace/checkpoint projection authority, and canonical global event-log
//! projection authority — see
//! `docs/rust-orchestrator/CODEX-TO-CLAUDE-RETURN-CONTINUATION.md` §7 for the
//! contract this module implements verbatim. It does not accept a live home,
//! an existing Go database, network authority, or a process executor, and it
//! wires no CLI command.
//!
//! Every public event's bytes are derived from a journal record's
//! `payload_json` plus its sealed [`EventProjectionRecipe`] — never from
//! `extra_json` directly, and never trusted from a caller assertion. Receipts
//! attest what a verified publisher ([`ProductionProjectionWriter`] or
//! [`CanonicalEventLog`]) actually wrote; they never choose identity.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "staged production surface exercised only by tests until a later cell wires \
                  the CLI composition root"
    )
)]

use crate::checkpoint_projection::{
    JournalCheckpointExpectation, checkpoint_for_state, journal_checkpoint_expectation,
};
#[cfg(unix)]
use crate::hermetic_provider::{
    DurableWorkerProjectionEvidence, R0DurableAttemptReport, R0DurableWorkerProjection,
    inspect_existing_r0_phase_start_durable_attempt, run_or_replay_r0_phase_start_durable_attempt,
};
use crate::{
    CanonicalEventLog, CanonicalEventLogError, CheckpointReconciliationDisposition,
    CompatibilityProjection, FreshFixtureAuthority, JournalIntent, ProductionBoundary,
    ProductionProjectionWriter, ProjectionReceipt, RuntimeStore, RuntimeStoreError,
    StorageActorAuthority, WorkspaceAuthority, WorkspaceError, WorkspaceSeed,
    lifecycle::LifecycleError,
    runtime_store::{EventProjectionRecipe, ProjectionRecoveryBounds, ProjectionRecoveryRecord},
};
use orchestrator_core::{
    CheckpointError, CheckpointPhase, CheckpointPlan, CheckpointProjection, CoreError, EventError,
    EventId, EventJsonMap, EventRecord, MissionId, MissionState, MissionStateBuildError,
    MissionStatus, PhaseDefinition, PhaseId, PhaseStatus, ReducerInput, ReducerTransition,
    TransitionError, VerificationClass, VerificationDecision, VerificationMode,
    VerificationOutcome, WorkerId, decide_verification, encode_current_checkpoint,
    encode_current_event, encode_current_plan, reduce,
};
use orchestrator_exec::{
    AttemptOutcome, ExecutionRequest, MechanicalTermination, WorkerEventCodecError,
    WorkerEventEnvelope, WorkerEventKind, WorkerEventPayload,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;

/// Sentinel transition kind for the one admission record every mission
/// carries. Never produced by [`event_type_str`], so it can never collide
/// with a real lifecycle event type.
const ADMISSION_TRANSITION_KIND: &str = "mission.admitted";

/// Dedicated recovery-snapshot ceilings, matching
/// `docs/rust-orchestrator/CODEX-TO-CLAUDE-RETURN-CONTINUATION.md` §7.3.
/// `RuntimeStore` enforces its own internal maxima; these are simply the
/// values this composition asks for.
const RECOVERY_MAX_RECORDS: usize = 10_000;
const RECOVERY_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Sequence bound used only by [`HermeticCompatibilityProjector::apply_transition`]'s
/// discarded, pre-append domain-validation reduction. Never a real allocated
/// public sequence (those are transactional counters starting at 1 and
/// advancing one at a time), so it can never collide with
/// `state.applied_sequences` or trip the reducer's out-of-order check for a
/// genuinely fresh transition.
const SPECULATIVE_VALIDATION_SEQUENCE: i64 = i64::MAX;

/// Builds the deliberately-fake event identifier for the same discarded,
/// pre-append validation reduction. `speculative-` can never collide with a
/// real recipe's `evt_`-prefixed identity (see `runtime_store.rs`'s
/// `derive_event_projection_id`, which this function does not call or
/// duplicate — `store.append` remains the only place allowed to mint a real
/// recipe).
fn speculative_event_id(transition_id: &str) -> String {
    format!("speculative-{}", hex_sha256(transition_id.as_bytes()))
}

/// Fail-closed errors from the hermetic compatibility projector.
#[derive(Debug, Error)]
pub(crate) enum HermeticProjectorError {
    #[error(transparent)]
    Store(#[from] RuntimeStoreError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    EventLog(#[from] CanonicalEventLogError),
    #[error(transparent)]
    Checkpoint(#[from] CheckpointError),
    #[error(transparent)]
    Event(#[from] EventError),
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error(transparent)]
    MissionBuild(#[from] MissionStateBuildError),
    #[error(transparent)]
    Transition(#[from] TransitionError),
    #[error(transparent)]
    CheckpointMapping(#[from] LifecycleError),
    #[error("hermetic projector does not support this lifecycle transition kind")]
    UnsupportedTransition,
    #[error("phase completion requires a structured PASS verification gate")]
    VerificationNotPassed,
    #[error("lifecycle event data must be a JSON object or null")]
    InvalidEventData,
    #[error("a journal record requiring an event-log projection has no sealed event recipe")]
    MissingEventRecipe,
    #[error("stored journal payload does not match the hermetic projector's payload shape")]
    CorruptJournalPayload,
    #[error("canonical worker projection requires exactly one running phase")]
    WorkerRunningPhaseRequired,
    #[error(
        "canonical worker projection request does not match the recovered mission/phase/runtime binding"
    )]
    WorkerRequestBindingMismatch,
    #[error("canonical worker journal prefix is malformed or conflicts with its bound attempt")]
    WorkerStreamConflict,
    #[error("terminal lifecycle transition is blocked by an active worker attempt")]
    ActiveWorkerAttempt,
    #[error("recovered journal transition identity does not match its canonical binding")]
    TransitionIdentityConflict,
    #[error(transparent)]
    WorkerEventCodec(#[from] WorkerEventCodecError),
    #[error("recovered journal record uses transition kind {0:?}, which is not recognized")]
    UnrecognizedTransitionKind(String),
    #[error(
        "compatibility projection {0:?} still requires a durable write, but no production \
         projection writer is available for this mission's current state. \
         `ProductionProjectionWriter` can only be acquired against a pristine workspace \
         (empty event log, checkpoint status \"pending\"); once any lifecycle transition has \
         published history, `WorkspaceAuthority::into_production_projection_writer` fails \
         closed with `ProductionProjectionRecoveryUnavailable` for a freshly reopened process. \
         This is a genuine, pre-existing gap in `workspace.rs` (see its own \
         `production_projection_with_history_requires_recovery_coordinator` test) that Cell 2D \
         is not permitted to close by editing that file; recovering this specific case requires \
         a non-pristine writer-recovery constructor to be added to `workspace.rs` in a later cell."
    )]
    ProjectionRecoveryUnavailable(CompatibilityProjection),
    #[error("R0 injected crash cut after {0}")]
    R0InjectedCrashCut(&'static str),
}

/// Outcome of one applied (or replayed) lifecycle transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TransitionOutcome {
    pub(crate) replayed: bool,
    pub(crate) mission_status: MissionStatus,
}

/// Adapter-only input for one process outcome that has already crossed its
/// durable terminal boundary.
///
/// The fields are intentionally private. The sole production constructor
/// accepts the provider module's sealed durable-process evidence; ordinary
/// callers cannot accidentally project a raw [`AttemptOutcome`] as if it were
/// durable.
pub(crate) struct DurableWorkerProjectionInput<'a> {
    request: &'a ExecutionRequest,
    outcome: &'a AttemptOutcome,
    committed_at_utc: &'a str,
}

impl<'a> DurableWorkerProjectionInput<'a> {
    /// Constructs projection input only from the provider module's sealed,
    /// exact-ledger evidence. That module admits this value only after the
    /// terminal observation and matching terminal decision are both durable.
    #[cfg(unix)]
    fn from_provider_evidence(evidence: &'a DurableWorkerProjectionEvidence) -> Self {
        Self {
            request: evidence.request(),
            outcome: evidence.outcome(),
            committed_at_utc: evidence.committed_at_utc(),
        }
    }

    #[cfg(test)]
    fn from_test_outcome(
        request: &'a ExecutionRequest,
        outcome: &'a AttemptOutcome,
        committed_at_utc: &'a str,
    ) -> Self {
        Self {
            request,
            outcome,
            committed_at_utc,
        }
    }
}

/// Typed evidence for one canonical `worker.spawned` plus terminal projection.
pub(crate) struct CanonicalWorkerProjection {
    spawned: WorkerEventEnvelope,
    terminal: WorkerEventEnvelope,
    replayed: bool,
}

impl CanonicalWorkerProjection {
    #[must_use]
    pub(crate) const fn spawned(&self) -> &WorkerEventEnvelope {
        &self.spawned
    }

    #[must_use]
    pub(crate) const fn terminal(&self) -> &WorkerEventEnvelope {
        &self.terminal
    }

    #[must_use]
    pub(crate) const fn replayed(&self) -> bool {
        self.replayed
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CanonicalWorkerAttempt {
    phase_id: PhaseId,
    worker_id: String,
    attempt: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CanonicalWorkerStream {
    Spawned,
    Completed,
    Failed,
}

/// Caller-supplied, immutable mission admission material.
///
/// `plan` is the single source of both the reducer's phase definitions and
/// the published `plan.json` bytes (via [`encode_current_plan`]), so the two
/// can never drift apart.
pub(crate) struct MissionSeed {
    pub(crate) mission_markdown: Vec<u8>,
    pub(crate) plan: CheckpointPlan,
}

/// The only composition that simultaneously owns `RuntimeStore` writer
/// authority, production workspace/checkpoint projection authority, and
/// canonical event-log projection authority for one mission.
pub(crate) struct HermeticCompatibilityProjector {
    store: RuntimeStore,
    boundary: Arc<ProductionBoundary>,
    mission_id: MissionId,
    mission_markdown: Vec<u8>,
    plan: CheckpointPlan,
    state: MissionState,
    checkpoint_template: CheckpointProjection,
    checkpoint_bytes: Vec<u8>,
    event_log: CanonicalEventLog,
    checkpoint_writer: Option<ProductionProjectionWriter>,
    last_checkpoint_disposition: Option<CheckpointReconciliationDisposition>,
    /// Lifecycle transition IDs already known durable (from recovery replay
    /// or a prior successful call to [`Self::apply_transition`] in this same
    /// live instance). Used only to distinguish a genuine retry (skip
    /// pre-append domain validation; the identity/fingerprint check inside
    /// `reduce` already handles it) from a genuinely fresh transition (must
    /// be validated before it is ever journaled) — see
    /// [`Self::apply_transition`]'s doc comment.
    applied_transition_ids: BTreeSet<String>,
    canonical_worker_streams: BTreeMap<CanonicalWorkerAttempt, CanonicalWorkerStream>,
    /// Armed only by the capability-bound R0 subprocess fixture. Production
    /// construction always leaves this `None`.
    r0_crash_point: Option<R0ProjectionCrashPoint>,
}

/// Every field [`HermeticCompatibilityProjector::open`] (and its
/// bounds-parameterized test sibling) produce before the retained
/// [`RuntimeStore`] writer is folded in. Kept separate from the assembled
/// projector so the store can be closed best-effort on any error path
/// without needing two different owners of the same `RuntimeStore` value.
struct OpenedFields {
    boundary: Arc<ProductionBoundary>,
    mission_id: MissionId,
    mission_markdown: Vec<u8>,
    plan: CheckpointPlan,
    state: MissionState,
    checkpoint_template: CheckpointProjection,
    checkpoint_bytes: Vec<u8>,
    event_log: CanonicalEventLog,
    checkpoint_writer: Option<ProductionProjectionWriter>,
    last_checkpoint_disposition: Option<CheckpointReconciliationDisposition>,
    applied_transition_ids: BTreeSet<String>,
    canonical_worker_streams: BTreeMap<CanonicalWorkerAttempt, CanonicalWorkerStream>,
}

impl OpenedFields {
    fn into_projector(self, store: RuntimeStore) -> HermeticCompatibilityProjector {
        HermeticCompatibilityProjector {
            store,
            boundary: self.boundary,
            mission_id: self.mission_id,
            mission_markdown: self.mission_markdown,
            plan: self.plan,
            state: self.state,
            checkpoint_template: self.checkpoint_template,
            checkpoint_bytes: self.checkpoint_bytes,
            event_log: self.event_log,
            checkpoint_writer: self.checkpoint_writer,
            last_checkpoint_disposition: self.last_checkpoint_disposition,
            applied_transition_ids: self.applied_transition_ids,
            canonical_worker_streams: self.canonical_worker_streams,
            r0_crash_point: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum R0ProjectionCrashPoint {
    JournalCommit,
    EventPublish,
    CheckpointPublish,
}

impl R0ProjectionCrashPoint {
    const fn label(self) -> &'static str {
        match self {
            Self::JournalCommit => "journal-commit",
            Self::EventPublish => "event-publish",
            Self::CheckpointPublish => "checkpoint-publish",
        }
    }
}

fn r0_crash_if(
    armed: Option<R0ProjectionCrashPoint>,
    reached: R0ProjectionCrashPoint,
) -> Result<(), HermeticProjectorError> {
    if armed == Some(reached) {
        Err(HermeticProjectorError::R0InjectedCrashCut(reached.label()))
    } else {
        Ok(())
    }
}

impl fmt::Debug for HermeticCompatibilityProjector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HermeticCompatibilityProjector")
            .field("mission_id", &self.mission_id.as_str())
            .field("mission_status", &self.state.status())
            .field(
                "checkpoint_writer_available",
                &self.checkpoint_writer.is_some(),
            )
            .finish()
    }
}

impl HermeticCompatibilityProjector {
    /// Opens (or admits, if new) one mission's hermetic compatibility
    /// projection, completing any pending projection work left over from an
    /// earlier crash before returning.
    ///
    /// Admission is naturally idempotent: re-calling this with the same
    /// `mission_id` and `seed` content resumes the same mission. Re-calling
    /// it with different content under the same mission fails closed via
    /// [`RuntimeStoreError::TransitionConflict`].
    ///
    /// This composition mints its own [`StorageActorAuthority`] — it is the
    /// single mission/storage actor that token is reserved for, so callers
    /// never construct or hand one in.
    ///
    /// ## Retry semantics on an error exit
    ///
    /// Every error path after [`RuntimeStore::open`] succeeds releases the
    /// writer lease via a best-effort [`RuntimeStore::close`] before
    /// returning the *original* error — `close`'s own failure is swallowed
    /// deliberately so the caller always learns why `open` failed, never why
    /// the best-effort cleanup did. This means a caller may retry `open` on
    /// the same boundary root within the same process without ever seeing
    /// [`RuntimeStoreError::WriterLeased`] purely as a residue of this
    /// method's own prior failure (e.g.
    /// [`HermeticProjectorError::ProjectionRecoveryUnavailable`], which
    /// since the non-pristine writer-recovery constructor landed fires only
    /// on genuine divergence or foreign fixture history, not on any
    /// non-pristine reopen).
    ///
    /// The one residual case this does *not* close: if the best-effort
    /// `close()` itself fails (for example, a checkpoint or fsync failure
    /// while trying to clean up), [`RuntimeStore`]'s own `Drop` deliberately
    /// keeps the lease held for the rest of the process's lifetime — that is
    /// pre-existing, intentional design (see its own doc comment) that this
    /// method does not attempt to override; a second `open` in that narrow
    /// case still observes `WriterLeased`.
    pub(crate) fn open(
        boundary: Arc<ProductionBoundary>,
        mission_id: MissionId,
        seed: MissionSeed,
        committed_at_utc: String,
    ) -> Result<Self, HermeticProjectorError> {
        let bounds = ProjectionRecoveryBounds::new(RECOVERY_MAX_RECORDS, RECOVERY_MAX_BYTES)?;
        Self::open_with_bounds(boundary, mission_id, seed, committed_at_utc, bounds)
    }

    /// Test-only sibling of [`Self::open`] that accepts a caller-supplied
    /// recovery-snapshot bound instead of the production ceiling in
    /// [`RECOVERY_MAX_RECORDS`]/[`RECOVERY_MAX_BYTES`], so a bounds-exhaustion
    /// test can prove `RuntimeStoreError::RecoveryBoundsExceeded` surfaces
    /// through [`HermeticProjectorError::Store`] without generating a
    /// production-scale journal.
    #[cfg(test)]
    pub(crate) fn open_with_recovery_bounds(
        boundary: Arc<ProductionBoundary>,
        mission_id: MissionId,
        seed: MissionSeed,
        committed_at_utc: String,
        bounds: ProjectionRecoveryBounds,
    ) -> Result<Self, HermeticProjectorError> {
        Self::open_with_bounds(boundary, mission_id, seed, committed_at_utc, bounds)
    }

    fn open_with_bounds(
        boundary: Arc<ProductionBoundary>,
        mission_id: MissionId,
        seed: MissionSeed,
        committed_at_utc: String,
        bounds: ProjectionRecoveryBounds,
    ) -> Result<Self, HermeticProjectorError> {
        let mut store = RuntimeStore::open(Arc::clone(&boundary), StorageActorAuthority::new())?;
        match Self::open_body(
            &mut store,
            &boundary,
            &mission_id,
            seed,
            committed_at_utc,
            bounds,
        ) {
            Ok(fields) => Ok(fields.into_projector(store)),
            Err(error) => {
                // Best-effort: close (releasing the writer lease) so a
                // subsequent `open()` on the same boundary root within this
                // process does not fail with `WriterLeased` purely as a
                // residue of this failed attempt. `close`'s own error is
                // deliberately discarded — the original error is always what
                // the caller learns; see this method's doc comment for the
                // one residual case (`close` itself failing) this cannot
                // close.
                let _ = store.close();
                Err(error)
            }
        }
    }

    /// The fallible body of `open`, run against an already-opened store
    /// borrowed for the duration of recovery. Returning `Result<OpenedFields,
    /// _>` rather than `Result<Self, _>` means every `?` in this function
    /// leaves the store itself in its caller's hands on any error, so
    /// [`Self::open_with_bounds`] can always attempt a clean close before
    /// propagating the error.
    fn open_body(
        store: &mut RuntimeStore,
        boundary: &Arc<ProductionBoundary>,
        mission_id: &MissionId,
        seed: MissionSeed,
        committed_at_utc: String,
        bounds: ProjectionRecoveryBounds,
    ) -> Result<OpenedFields, HermeticProjectorError> {
        let MissionSeed {
            mission_markdown,
            plan,
        } = seed;

        let pristine_state = build_pristine_state(mission_id, &plan)?;
        let template = CheckpointProjection {
            workspace_id: mission_id.as_str().to_owned(),
            plan: Some(plan.clone()),
            ..CheckpointProjection::default()
        };
        let initial_checkpoint = checkpoint_for_state(&template, &pristine_state)?;
        let checkpoint_bytes = encode_current_checkpoint(&initial_checkpoint)?;
        let plan_bytes = encode_current_plan(&plan)?;

        let admission_payload = serde_json::json!({
            "mission_sha256": hex_sha256(&mission_markdown),
            "plan_sha256": hex_sha256(&plan_bytes),
        });
        let admission_intent = JournalIntent::new(
            admission_transition_id(mission_id),
            Some(mission_id.clone()),
            ADMISSION_TRANSITION_KIND,
            admission_payload,
            committed_at_utc,
        )?
        .with_required_projection(CompatibilityProjection::Workspace)
        .with_required_projection(CompatibilityProjection::Checkpoint);
        store.append(&admission_intent)?;

        let seed_workspace = WorkspaceSeed::new(
            mission_markdown.clone(),
            &initial_checkpoint,
            plan_bytes.clone(),
        )?;
        let workspace_authority = match WorkspaceAuthority::create_production(
            Arc::clone(boundary),
            mission_id.clone(),
            seed_workspace,
        ) {
            Ok(authority) => authority,
            Err(WorkspaceError::Collision(_)) => {
                WorkspaceAuthority::admit_production(Arc::clone(boundary), mission_id.clone())?
            }
            Err(error) => return Err(error.into()),
        };
        // Fast path for the overwhelmingly common case (a brand new mission,
        // or a reopen that never crashed past its first checkpoint advance):
        // the pristine constructor is unchanged and still tried first here,
        // exactly as it always has been. `checkpoint_writer` is `mut` only
        // because the replay loop below may still need to *lazily* upgrade
        // it to a recovered writer (see the loop's own comment) — this
        // eager attempt's behavior and the writer it produces are identical
        // to before.
        let mut checkpoint_writer = match workspace_authority.into_production_projection_writer() {
            Ok(writer) => Some(writer),
            Err(WorkspaceError::ProductionProjectionRecoveryUnavailable) => None,
            Err(error) => return Err(error.into()),
        };

        let mut event_log =
            CanonicalEventLog::open_production(Arc::clone(boundary), mission_id.clone())?;

        let snapshot = store.projection_recovery_snapshot(mission_id, &bounds)?;

        let mut state = pristine_state;
        let mut checkpoint_template = initial_checkpoint;
        let mut checkpoint_bytes_current = checkpoint_bytes.clone();
        let mut applied_transition_ids = BTreeSet::new();
        let mut canonical_worker_streams = BTreeMap::new();
        let mut last_checkpoint_disposition = None;

        for record in snapshot.records() {
            if record.transition_kind() == ADMISSION_TRANSITION_KIND {
                if record.transition_id() != admission_transition_id(mission_id) {
                    return Err(HermeticProjectorError::TransitionIdentityConflict);
                }
                // No lazy upgrade attempted here: admission's own required
                // checkpoint projection is only ever *verified*
                // (`ensure_admission_receipts`'s `Some(writer)` arm calls
                // `verify_exact_base`, never `reconcile_checkpoint`) — a
                // writer cannot help produce a byte this composition would
                // never write through admission in the first place. If the
                // durable checkpoint does not already match admission's
                // pristine target exactly, no writer, recovered or
                // otherwise, changes that outcome.
                ensure_admission_receipts(
                    store,
                    record,
                    checkpoint_writer.as_ref(),
                    boundary,
                    mission_id,
                    &mission_markdown,
                    &plan,
                    &checkpoint_bytes,
                )?;
                continue;
            }

            let recipe = record
                .event_projection_recipe()
                .ok_or(HermeticProjectorError::MissingEventRecipe)?
                .clone();
            let payload = decode_payload(record.payload_json())?;
            let event_record = build_projection_event_record(
                mission_id,
                record.transition_kind(),
                record.committed_at_utc(),
                &recipe,
                payload.phase_id.as_ref(),
                payload.worker_id.as_deref(),
                &payload.data,
                payload.reason.as_deref(),
            )?;
            let recovered_worker = recovered_worker_attempt(
                boundary,
                &state,
                record.transition_id(),
                record.transition_kind(),
                &event_record,
                &canonical_worker_streams,
            )?;
            let transition = if recovered_worker.is_some() {
                if payload.reason.is_some() {
                    return Err(HermeticProjectorError::CorruptJournalPayload);
                }
                ReducerTransition::Unknown {
                    event_type: record.transition_kind().to_owned(),
                    data: payload.data.clone(),
                }
            } else {
                if payload.worker_id.is_some() {
                    return Err(HermeticProjectorError::CorruptJournalPayload);
                }
                let transition_id = lifecycle_transition_id(
                    mission_id,
                    payload.phase_id.as_ref(),
                    record.transition_kind(),
                );
                if record.transition_id() != transition_id {
                    return Err(HermeticProjectorError::TransitionIdentityConflict);
                }
                // Every durable lifecycle record — receipted or not — is a
                // transition this composition has already committed to.
                // Track it so a later live `apply_transition` call recognizes
                // an exact retry rather than re-validating it against state
                // that has already moved past it.
                applied_transition_ids.insert(transition_id);
                parse_transition_kind(record.transition_kind(), payload.reason.clone())?
            };
            ensure_lifecycle_worker_boundary(
                &canonical_worker_streams,
                payload.phase_id.as_ref(),
                &transition,
            )?;
            let reducer_input = ReducerInput {
                event_id: EventId::new(recipe.event_id())?,
                sequence: recipe.public_sequence(),
                timestamp: record.committed_at_utc().to_owned(),
                mission_id: mission_id.clone(),
                phase_id: payload.phase_id.clone(),
                worker_id: payload.worker_id.clone(),
                data: payload.data.clone(),
                extra: BTreeMap::new(),
                transition,
            };
            let reduction = reduce(&state, &reducer_input)?;
            let target_checkpoint = checkpoint_for_state(&checkpoint_template, &reduction.state)?;
            let target_checkpoint_bytes = encode_current_checkpoint(&target_checkpoint)?;

            if record.missing_projections().is_empty() {
                // Already receipted: still cross-check the already-published
                // event against journal truth rather than trusting it, so a
                // tampered (same-shape, different-content) history fails
                // closed here instead of silently being trusted.
                verify_already_published_event(&event_log, &recipe, &event_record)?;
            } else {
                // `publish_and_receipt` performs its own lazy writer upgrade
                // (see that function) exactly when it discovers a real write
                // is still needed — i.e. only after it has already confirmed
                // the durable checkpoint is *not* already at `target` (the
                // legitimate crossed-crash-boundary case, which needs no
                // writer at all). Attempting the upgrade any earlier, purely
                // because `checkpoint_writer` is `None` here, would wrongly
                // reject that legitimate case: this record's own "prior"
                // (`checkpoint_bytes_current`) is not what's on disk once a
                // prior crash already completed this exact write.
                last_checkpoint_disposition = None;
                publish_and_receipt(
                    store,
                    &mut event_log,
                    &mut checkpoint_writer,
                    &mut last_checkpoint_disposition,
                    boundary,
                    mission_id,
                    record.journal_sequence(),
                    &recipe,
                    &event_record,
                    &checkpoint_template,
                    &state,
                    &checkpoint_bytes_current,
                    &target_checkpoint_bytes,
                    record.committed_at_utc(),
                    None,
                )?;
            }

            state = reduction.state;
            checkpoint_template = target_checkpoint;
            checkpoint_bytes_current = target_checkpoint_bytes;
            if let Some((attempt, stream)) = recovered_worker {
                canonical_worker_streams.insert(attempt, stream);
            }
        }

        // Final defense-in-depth: the fully-replayed checkpoint target must
        // equal whatever is durably on disk right now, regardless of which
        // records above needed a fresh publish. This is the projector's own
        // divergent-checkpoint adversarial check, independent of whichever
        // publisher wrote each byte. Reads through the same guarded,
        // mode/link-checked discipline `verify_exact_base` uses (Cell 2D
        // Fix 3), not the weaker no-follow-only accessor.
        let durable_checkpoint =
            WorkspaceAuthority::admit_production(Arc::clone(boundary), mission_id.clone())?
                .verified_lifecycle_checkpoint_bytes()?;
        if durable_checkpoint != checkpoint_bytes_current {
            return Err(HermeticProjectorError::ProjectionRecoveryUnavailable(
                CompatibilityProjection::Checkpoint,
            ));
        }

        Ok(OpenedFields {
            boundary: Arc::clone(boundary),
            mission_id: mission_id.clone(),
            mission_markdown,
            plan,
            state,
            checkpoint_template,
            checkpoint_bytes: checkpoint_bytes_current,
            event_log,
            checkpoint_writer,
            last_checkpoint_disposition,
            applied_transition_ids,
            canonical_worker_streams,
        })
    }

    #[must_use]
    pub(crate) fn mission_id(&self) -> &MissionId {
        &self.mission_id
    }

    #[must_use]
    pub(crate) const fn state(&self) -> &MissionState {
        &self.state
    }

    #[must_use]
    pub(crate) fn checkpoint_bytes(&self) -> &[u8] {
        &self.checkpoint_bytes
    }

    #[must_use]
    pub(crate) fn mission_markdown(&self) -> &[u8] {
        &self.mission_markdown
    }

    #[must_use]
    pub(crate) fn plan(&self) -> &CheckpointPlan {
        &self.plan
    }

    /// True once this projector holds a production checkpoint-projection
    /// writer. `false` no longer implies a future checkpoint write must
    /// fail: `publish_and_receipt` lazily upgrades via
    /// `try_recover_checkpoint_writer` (byte-verified non-pristine
    /// recovery) at the moment a real write is proven necessary.
    /// [`HermeticProjectorError::ProjectionRecoveryUnavailable`] now fires
    /// only when that recovery proof itself fails — genuine divergence or
    /// foreign fixture history.
    #[must_use]
    pub(crate) const fn has_checkpoint_writer(&self) -> bool {
        self.checkpoint_writer.is_some()
    }

    /// Reduces, journals, and durably projects one lifecycle transition,
    /// following the journal-first order in
    /// `docs/rust-orchestrator/CODEX-TO-CLAUDE-RETURN-CONTINUATION.md` §7.6:
    /// verification gate, semantic reduction, allocate and journal exact
    /// event identity, publish/verify the event, record its receipt,
    /// publish/verify the checkpoint, record its receipt, and only then
    /// expose the new in-memory state.
    ///
    /// ## Why the transition is reduced before it is ever journaled
    ///
    /// `store.append` is the *only* place in the crate allowed to allocate a
    /// transition's real [`EventProjectionRecipe`] (event ID + public
    /// sequence) — it is a transactional counter, not something this method
    /// may predict. That means a genuinely fresh transition cannot be reduced
    /// with its real identity until *after* it is already durably journaled
    /// with `EventLog`/`Checkpoint` required. If that reduction then failed
    /// domain validation (e.g. `PhaseCompleted` on a phase that was never
    /// started), the journal row would be permanent and unfulfillable: this
    /// composition can never publish an event/checkpoint for a transition
    /// `reduce` rejects, so every future `open()` replaying that row would
    /// fail identically, forever.
    ///
    /// To validate *before* committing to a journal row, this method reduces
    /// once against `self.state` using a deliberately out-of-band identity
    /// (sequence `i64::MAX`, a `speculative-`-prefixed event ID that can
    /// never collide with a real `evt_`-prefixed one) purely to exercise
    /// `reduce`'s domain checks; that `Reduction` is discarded immediately —
    /// it never touches `self.state`, the checkpoint, or the journal. This
    /// speculative check is skipped for a transition already known durable
    /// (`self.applied_transition_ids`, populated by `open`'s recovery replay
    /// and by this method's own prior successes): an exact retry's domain
    /// legality was already established by the real reduction that first
    /// applied it, and re-running the speculative check against
    /// already-advanced state would wrongly reject a legitimate replay (the
    /// mission has since moved past it). The single *authoritative* `reduce`
    /// call — the one whose output feeds both the checkpoint target and
    /// `self.state` — still runs exactly once, after `store.append`, using
    /// the real sealed identity; this method never reduces twice against the
    /// real identity.
    ///
    /// Exact retries (the same mission/phase/transition-kind pair, with the
    /// same `committed_at_utc`) are idempotent: [`RuntimeStore::append`],
    /// [`CanonicalEventLog::publish_exact_next_line`], and
    /// [`ProductionProjectionWriter::reconcile_checkpoint`] each treat a
    /// crossed crash boundary as a verified no-op, so this method never
    /// needs its own separate pending-recovery bookkeeping.
    pub(crate) fn apply_transition(
        &mut self,
        phase_id: Option<PhaseId>,
        transition: ReducerTransition,
        data: Value,
        verification: Option<VerificationDecision>,
        committed_at_utc: String,
    ) -> Result<TransitionOutcome, HermeticProjectorError> {
        if matches!(
            transition,
            ReducerTransition::PhaseRetrying | ReducerTransition::Unknown { .. }
        ) {
            return Err(HermeticProjectorError::UnsupportedTransition);
        }
        if matches!(transition, ReducerTransition::PhaseCompleted)
            && !verification.is_some_and(VerificationDecision::gate_passed)
        {
            return Err(HermeticProjectorError::VerificationNotPassed);
        }
        ensure_lifecycle_worker_boundary(
            &self.canonical_worker_streams,
            phase_id.as_ref(),
            &transition,
        )?;

        let event_type = event_type_str(&transition);
        let reason = transition_reason(&transition).map(str::to_owned);
        let payload = encode_payload(phase_id.as_ref(), &data, reason.as_deref())?;
        let transition_id =
            lifecycle_transition_id(&self.mission_id, phase_id.as_ref(), event_type);

        if !self.applied_transition_ids.contains(&transition_id) {
            let speculative_input = ReducerInput {
                event_id: EventId::new(speculative_event_id(&transition_id))?,
                sequence: SPECULATIVE_VALIDATION_SEQUENCE,
                timestamp: committed_at_utc.clone(),
                mission_id: self.mission_id.clone(),
                phase_id: phase_id.clone(),
                worker_id: None,
                data: data.clone(),
                extra: BTreeMap::new(),
                transition: transition.clone(),
            };
            // Discarded: this call exists only to surface a domain
            // `TransitionError` before the transition is ever journaled. See
            // this method's doc comment.
            reduce(&self.state, &speculative_input)?;
        }

        let intent = JournalIntent::new(
            transition_id.clone(),
            Some(self.mission_id.clone()),
            event_type,
            payload,
            committed_at_utc.clone(),
        )?
        .with_required_projection(CompatibilityProjection::EventLog)
        .with_required_projection(CompatibilityProjection::Checkpoint);
        let commit = self.store.append(&intent)?;
        r0_crash_if(self.r0_crash_point, R0ProjectionCrashPoint::JournalCommit)?;
        let recipe = commit
            .event_projection_recipe()
            .ok_or(HermeticProjectorError::MissingEventRecipe)?
            .clone();

        let reducer_input = ReducerInput {
            event_id: EventId::new(recipe.event_id())?,
            sequence: recipe.public_sequence(),
            timestamp: committed_at_utc.clone(),
            mission_id: self.mission_id.clone(),
            phase_id: phase_id.clone(),
            worker_id: None,
            data: data.clone(),
            extra: BTreeMap::new(),
            transition,
        };
        let reduction = reduce(&self.state, &reducer_input)?;
        let event_record = build_event_record(
            &self.mission_id,
            event_type,
            &committed_at_utc,
            &recipe,
            phase_id.as_ref(),
            &data,
            reason.as_deref(),
        )?;
        let target_checkpoint = checkpoint_for_state(&self.checkpoint_template, &reduction.state)?;
        let target_checkpoint_bytes = encode_current_checkpoint(&target_checkpoint)?;

        self.last_checkpoint_disposition = None;
        publish_and_receipt(
            &mut self.store,
            &mut self.event_log,
            &mut self.checkpoint_writer,
            &mut self.last_checkpoint_disposition,
            &self.boundary,
            &self.mission_id,
            commit.sequence(),
            &recipe,
            &event_record,
            &self.checkpoint_template,
            &self.state,
            &self.checkpoint_bytes,
            &target_checkpoint_bytes,
            &committed_at_utc,
            self.r0_crash_point,
        )?;

        self.state = reduction.state;
        self.checkpoint_template = target_checkpoint;
        self.checkpoint_bytes = target_checkpoint_bytes;
        self.applied_transition_ids.insert(transition_id);

        Ok(TransitionOutcome {
            replayed: commit.duplicate(),
            mission_status: self.state.status(),
        })
    }

    /// Projects the canonical two-record worker stream for one already
    /// durable process outcome.
    ///
    /// Identity is not accepted independently. The mission and sole running
    /// phase come from journal-reconstructed state, the worker ID is derived
    /// with [`WorkerId::for_phase`] from the validated request persona, and
    /// the attempt/runtime/payload come from that same validated request and
    /// typed outcome. The adapter-only input can be constructed in production
    /// solely from the provider module's sealed durable-process evidence.
    pub(crate) fn project_durable_worker_attempt(
        &mut self,
        input: DurableWorkerProjectionInput<'_>,
    ) -> Result<CanonicalWorkerProjection, HermeticProjectorError> {
        let binding = self.bind_worker_attempt(input.request)?;
        let spawned_data = worker_spawned_data(input.request, binding.attempt);
        let spawned_at = if self.canonical_worker_streams.contains_key(&binding) {
            self.existing_worker_envelope(&binding, WorkerEventKind::Spawned)?
                .receipt()
                .timestamp()
                .to_owned()
        } else {
            input.committed_at_utc.to_owned()
        };
        let (spawned, spawned_replayed) = self.project_canonical_worker_event(
            &binding,
            WorkerEventKind::Spawned,
            spawned_data,
            &spawned_at,
        )?;
        let (terminal_kind, terminal_data) = worker_terminal_data(input.outcome, binding.attempt);
        let (terminal, terminal_replayed) = self.project_canonical_worker_event(
            &binding,
            terminal_kind,
            terminal_data,
            input.committed_at_utc,
        )?;
        Ok(CanonicalWorkerProjection {
            spawned,
            terminal,
            replayed: spawned_replayed && terminal_replayed,
        })
    }

    fn bind_worker_attempt(
        &self,
        request: &ExecutionRequest,
    ) -> Result<CanonicalWorkerAttempt, HermeticProjectorError> {
        let phase_id = sole_running_phase_id(&self.state)
            .ok_or(HermeticProjectorError::WorkerRunningPhaseRequired)?;
        if request.mission() != self.mission_id.as_str()
            || request.phase() != phase_id.as_str()
            || request.attempt() == 0
            || request.runtime().as_str().is_empty()
        {
            return Err(HermeticProjectorError::WorkerRequestBindingMismatch);
        }
        let workspace = WorkspaceAuthority::admit_production(
            Arc::clone(&self.boundary),
            self.mission_id.clone(),
        )?;
        let phase_worker = match workspace.phase_worker_binding(
            request.persona(),
            &phase_id,
            request.worker_dir(),
        ) {
            Ok(binding) => binding,
            Err(WorkspaceError::ProcessWorkingRootMismatch) => {
                return Err(HermeticProjectorError::WorkerRequestBindingMismatch);
            }
            Err(error) => return Err(error.into()),
        };
        Ok(CanonicalWorkerAttempt {
            phase_id,
            worker_id: phase_worker.worker_id().as_str().to_owned(),
            attempt: request.attempt(),
        })
    }

    fn existing_worker_envelope(
        &self,
        binding: &CanonicalWorkerAttempt,
        kind: WorkerEventKind,
    ) -> Result<WorkerEventEnvelope, HermeticProjectorError> {
        let mut found = None;
        for event in self.event_log.events().iter().filter(|event| {
            event.record.event_type == kind.as_go_str()
                && event.record.mission_id == self.mission_id.as_str()
                && event.record.phase_id.as_deref() == Some(binding.phase_id.as_str())
                && event.record.worker_id.as_deref() == Some(binding.worker_id.as_str())
        }) {
            let envelope = worker_envelope_from_record(&event.record)?;
            if envelope.attempt() != Some(binding.attempt) {
                continue;
            }
            if found.replace(envelope).is_some() {
                return Err(HermeticProjectorError::WorkerStreamConflict);
            }
        }
        let envelope = found.ok_or(HermeticProjectorError::WorkerStreamConflict)?;
        ensure_worker_envelope_binding(&envelope, &self.mission_id, binding, kind)?;
        Ok(envelope)
    }

    fn project_canonical_worker_event(
        &mut self,
        binding: &CanonicalWorkerAttempt,
        kind: WorkerEventKind,
        data: Value,
        committed_at_utc: &str,
    ) -> Result<(WorkerEventEnvelope, bool), HermeticProjectorError> {
        let next_stream = next_worker_stream(&self.canonical_worker_streams, binding, kind)?;
        let transition_id = worker_transition_id(&self.mission_id, binding, kind);
        let payload = encode_worker_payload(binding, &data)?;
        let intent = JournalIntent::new(
            transition_id,
            Some(self.mission_id.clone()),
            kind.as_go_str(),
            payload,
            committed_at_utc.to_owned(),
        )?
        .with_required_projection(CompatibilityProjection::EventLog)
        .with_required_projection(CompatibilityProjection::Checkpoint);
        let commit = self.store.append(&intent)?;
        r0_crash_if(self.r0_crash_point, R0ProjectionCrashPoint::JournalCommit)?;
        let recipe = commit
            .event_projection_recipe()
            .ok_or(HermeticProjectorError::MissingEventRecipe)?
            .clone();
        let event_record = build_projection_event_record(
            &self.mission_id,
            kind.as_go_str(),
            committed_at_utc,
            &recipe,
            Some(&binding.phase_id),
            Some(&binding.worker_id),
            &data,
            None,
        )?;
        let envelope = worker_envelope_from_record(&event_record)?;
        ensure_worker_envelope_binding(&envelope, &self.mission_id, binding, kind)?;

        let event_id = EventId::new(recipe.event_id())?;
        let already_reduced = self.state.applied_event(&event_id).is_some();
        let transition_data = data.clone();
        let reducer_input = ReducerInput {
            event_id,
            sequence: recipe.public_sequence(),
            timestamp: committed_at_utc.to_owned(),
            mission_id: self.mission_id.clone(),
            phase_id: Some(binding.phase_id.clone()),
            worker_id: Some(binding.worker_id.clone()),
            data,
            extra: BTreeMap::new(),
            transition: ReducerTransition::Unknown {
                event_type: kind.as_go_str().to_owned(),
                data: transition_data,
            },
        };
        let reduction = reduce(&self.state, &reducer_input)?;
        if already_reduced {
            verify_already_published_event(&self.event_log, &recipe, &event_record)?;
        } else {
            let target_checkpoint =
                checkpoint_for_state(&self.checkpoint_template, &reduction.state)?;
            let target_checkpoint_bytes = encode_current_checkpoint(&target_checkpoint)?;
            self.last_checkpoint_disposition = None;
            publish_and_receipt(
                &mut self.store,
                &mut self.event_log,
                &mut self.checkpoint_writer,
                &mut self.last_checkpoint_disposition,
                &self.boundary,
                &self.mission_id,
                commit.sequence(),
                &recipe,
                &event_record,
                &self.checkpoint_template,
                &self.state,
                &self.checkpoint_bytes,
                &target_checkpoint_bytes,
                committed_at_utc,
                self.r0_crash_point,
            )?;
            self.checkpoint_template = target_checkpoint;
            self.checkpoint_bytes = target_checkpoint_bytes;
        }
        self.state = reduction.state;
        self.canonical_worker_streams
            .insert(binding.clone(), next_stream);
        Ok((envelope, commit.duplicate()))
    }

    /// Releases the retained runtime store writer lease.
    pub(crate) fn close(self) -> Result<(), HermeticProjectorError> {
        self.store.close().map_err(Into::into)
    }
}

fn next_worker_stream(
    streams: &BTreeMap<CanonicalWorkerAttempt, CanonicalWorkerStream>,
    binding: &CanonicalWorkerAttempt,
    kind: WorkerEventKind,
) -> Result<CanonicalWorkerStream, HermeticProjectorError> {
    let current = streams.get(binding).copied();
    if current.is_none() {
        ensure_new_worker_attempt_is_ordered(streams, binding)?;
    }
    match (current, kind) {
        (None, WorkerEventKind::Spawned) => Ok(CanonicalWorkerStream::Spawned),
        (Some(stream), WorkerEventKind::Spawned) => Ok(stream),
        (Some(CanonicalWorkerStream::Spawned), WorkerEventKind::Completed)
        | (Some(CanonicalWorkerStream::Completed), WorkerEventKind::Completed) => {
            Ok(CanonicalWorkerStream::Completed)
        }
        (Some(CanonicalWorkerStream::Spawned), WorkerEventKind::Failed)
        | (Some(CanonicalWorkerStream::Failed), WorkerEventKind::Failed) => {
            Ok(CanonicalWorkerStream::Failed)
        }
        (
            None | Some(CanonicalWorkerStream::Completed | CanonicalWorkerStream::Failed),
            WorkerEventKind::Completed | WorkerEventKind::Failed,
        )
        | (_, WorkerEventKind::Output) => Err(HermeticProjectorError::WorkerStreamConflict),
    }
}

fn ensure_new_worker_attempt_is_ordered(
    streams: &BTreeMap<CanonicalWorkerAttempt, CanonicalWorkerStream>,
    binding: &CanonicalWorkerAttempt,
) -> Result<(), HermeticProjectorError> {
    let mut greatest_attempt = None;
    for (stored, stream) in streams {
        if stored.phase_id != binding.phase_id || stored.worker_id != binding.worker_id {
            continue;
        }
        if *stream == CanonicalWorkerStream::Spawned {
            return Err(HermeticProjectorError::WorkerStreamConflict);
        }
        greatest_attempt = Some(
            greatest_attempt.map_or(stored.attempt, |greatest: u32| greatest.max(stored.attempt)),
        );
    }
    if greatest_attempt.is_some_and(|greatest| binding.attempt <= greatest) {
        return Err(HermeticProjectorError::WorkerStreamConflict);
    }
    Ok(())
}

fn sole_running_phase_id(state: &MissionState) -> Option<PhaseId> {
    if state.status() != MissionStatus::InProgress {
        return None;
    }
    let mut running = state
        .phases()
        .filter(|phase| phase.status == PhaseStatus::Running);
    let phase_id = running.next()?.id.clone();
    running.next().is_none().then_some(phase_id)
}

/// Keeps every canonical worker stream closeable: recovery accepts a worker
/// terminal only while its phase is still running, so no phase or mission
/// terminal may advance past a `Spawned` stream. Cancellation/failure must
/// persist `worker.failed` first, then terminate the lifecycle.
fn ensure_lifecycle_worker_boundary(
    streams: &BTreeMap<CanonicalWorkerAttempt, CanonicalWorkerStream>,
    phase_id: Option<&PhaseId>,
    transition: &ReducerTransition,
) -> Result<(), HermeticProjectorError> {
    let active_in_phase = |phase: &PhaseId| {
        streams.iter().any(|(attempt, stream)| {
            attempt.phase_id == *phase && *stream == CanonicalWorkerStream::Spawned
        })
    };
    let blocked = match transition {
        ReducerTransition::PhaseCompleted
        | ReducerTransition::PhaseFailed { .. }
        | ReducerTransition::PhaseSkipped { .. } => phase_id.is_some_and(active_in_phase),
        ReducerTransition::MissionCompleted
        | ReducerTransition::MissionFailed
        | ReducerTransition::MissionCancelled { .. } => streams
            .values()
            .any(|stream| *stream == CanonicalWorkerStream::Spawned),
        _ => false,
    };
    if blocked {
        Err(HermeticProjectorError::ActiveWorkerAttempt)
    } else {
        Ok(())
    }
}

fn worker_transition_id(
    mission_id: &MissionId,
    binding: &CanonicalWorkerAttempt,
    kind: WorkerEventKind,
) -> String {
    let mut digest = Sha256::new();
    let attempt = binding.attempt.to_be_bytes();
    let logical_slot = match kind {
        WorkerEventKind::Spawned => "spawned",
        WorkerEventKind::Completed | WorkerEventKind::Failed => "terminal",
        WorkerEventKind::Output => "output",
    };
    for field in [
        mission_id.as_str().as_bytes(),
        binding.phase_id.as_str().as_bytes(),
        binding.worker_id.as_bytes(),
        attempt.as_slice(),
        logical_slot.as_bytes(),
    ] {
        digest.update(u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
        digest.update(field);
    }
    format!("hermetic-worker:{}", hex_sha256(&digest.finalize()))
}

fn encode_worker_payload(
    binding: &CanonicalWorkerAttempt,
    data: &Value,
) -> Result<Value, HermeticProjectorError> {
    let Value::Object(_) = data else {
        return Err(HermeticProjectorError::InvalidEventData);
    };
    Ok(serde_json::json!({
        "phase_id": binding.phase_id.as_str(),
        "worker_id": binding.worker_id,
        "data": data,
    }))
}

fn worker_spawned_data(request: &ExecutionRequest, attempt: u32) -> Value {
    let mut data = serde_json::Map::new();
    if !request.model().is_empty() {
        data.insert(
            "model".to_owned(),
            Value::String(request.model().to_owned()),
        );
    }
    data.insert(
        "runtime".to_owned(),
        Value::String(request.runtime().as_str().to_owned()),
    );
    data.insert(
        "effort_level".to_owned(),
        Value::String(request.effort().as_str().to_owned()),
    );
    data.insert(
        "persona".to_owned(),
        Value::String(request.persona().to_owned()),
    );
    data.insert(
        "dir".to_owned(),
        Value::String(request.worker_dir().to_string_lossy().into_owned()),
    );
    data.insert("attempt".to_owned(), Value::from(attempt));
    Value::Object(data)
}

fn worker_terminal_data(outcome: &AttemptOutcome, attempt: u32) -> (WorkerEventKind, Value) {
    let duration = format_worker_duration(outcome.elapsed());
    match outcome {
        AttemptOutcome::Completed(completed) => (
            WorkerEventKind::Completed,
            serde_json::json!({
                "output_len": completed.final_output().len(),
                "duration": duration,
                "attempt": attempt,
            }),
        ),
        AttemptOutcome::Incomplete(incomplete) => {
            let error = incomplete.failures().first().map_or_else(
                || format!("attempt ended: {:?}", incomplete.termination()),
                |failure| failure.expose_detail().to_owned(),
            );
            let mut data = serde_json::Map::new();
            data.insert("error".to_owned(), Value::String(error));
            data.insert("duration".to_owned(), Value::String(duration));
            if let Some(output_len) = incomplete.partial_work().partial_output().map(str::len) {
                data.insert("output_len".to_owned(), Value::from(output_len));
            }
            if let MechanicalTermination::ProcessExited(status) = incomplete.termination() {
                if let Some(exit_code) = status.as_code() {
                    data.insert("exit_code".to_owned(), Value::from(exit_code));
                }
            }
            data.insert("attempt".to_owned(), Value::from(attempt));
            (WorkerEventKind::Failed, Value::Object(data))
        }
    }
}

fn format_worker_duration(duration: Duration) -> String {
    let nanos = duration.as_nanos();
    if nanos == 0 {
        "0s".to_owned()
    } else if nanos % 1_000_000_000 == 0 {
        format!("{}s", nanos / 1_000_000_000)
    } else {
        format!("{nanos}ns")
    }
}

fn worker_envelope_from_record(
    record: &EventRecord,
) -> Result<WorkerEventEnvelope, HermeticProjectorError> {
    let encoded = encode_current_event(record)?;
    let json =
        std::str::from_utf8(&encoded).map_err(|_| HermeticProjectorError::CorruptJournalPayload)?;
    Ok(WorkerEventEnvelope::from_json(json)?)
}

fn ensure_worker_envelope_binding(
    envelope: &WorkerEventEnvelope,
    mission_id: &MissionId,
    binding: &CanonicalWorkerAttempt,
    kind: WorkerEventKind,
) -> Result<(), HermeticProjectorError> {
    if envelope.identity().mission_id() != mission_id.as_str()
        || envelope.identity().phase_id() != binding.phase_id.as_str()
        || envelope.identity().worker_id() != binding.worker_id
        || envelope.attempt() != Some(binding.attempt)
        || envelope.kind() != kind
    {
        return Err(HermeticProjectorError::WorkerStreamConflict);
    }
    Ok(())
}

fn worker_event_kind(kind: &str) -> Option<WorkerEventKind> {
    match kind {
        "worker.spawned" => Some(WorkerEventKind::Spawned),
        "worker.completed" => Some(WorkerEventKind::Completed),
        "worker.failed" => Some(WorkerEventKind::Failed),
        _ => None,
    }
}

fn ensure_canonical_worker_data(
    record: &EventRecord,
    kind: WorkerEventKind,
) -> Result<(), HermeticProjectorError> {
    let data = record
        .data
        .as_ref()
        .ok_or(HermeticProjectorError::WorkerStreamConflict)?;
    let (required, allowed): (&[&str], &[&str]) = match kind {
        WorkerEventKind::Spawned => (
            &["runtime", "effort_level", "persona", "dir", "attempt"],
            &[
                "model",
                "runtime",
                "effort_level",
                "persona",
                "dir",
                "attempt",
            ],
        ),
        WorkerEventKind::Completed => (
            &["output_len", "duration", "attempt"],
            &["output_len", "duration", "attempt"],
        ),
        WorkerEventKind::Failed => (
            &["error", "duration", "attempt"],
            &["error", "duration", "output_len", "exit_code", "attempt"],
        ),
        WorkerEventKind::Output => return Err(HermeticProjectorError::WorkerStreamConflict),
    };
    if required.iter().any(|field| !data.contains_key(field))
        || data
            .iter()
            .any(|(field, _)| !allowed.contains(&field.as_str()))
    {
        return Err(HermeticProjectorError::WorkerStreamConflict);
    }
    Ok(())
}

fn recovered_worker_attempt(
    boundary: &Arc<ProductionBoundary>,
    state: &MissionState,
    transition_id: &str,
    transition_kind: &str,
    event_record: &EventRecord,
    streams: &BTreeMap<CanonicalWorkerAttempt, CanonicalWorkerStream>,
) -> Result<Option<(CanonicalWorkerAttempt, CanonicalWorkerStream)>, HermeticProjectorError> {
    let Some(kind) = worker_event_kind(transition_kind) else {
        if transition_kind == "worker.output" {
            return Err(HermeticProjectorError::WorkerStreamConflict);
        }
        return Ok(None);
    };
    if !event_record.extra.is_empty() {
        return Err(HermeticProjectorError::WorkerStreamConflict);
    }
    ensure_canonical_worker_data(event_record, kind)?;
    let envelope = worker_envelope_from_record(event_record)?;
    let phase_id = PhaseId::new(envelope.identity().phase_id().to_owned())?;
    WorkerId::new(envelope.identity().worker_id().to_owned())?;
    if sole_running_phase_id(state).as_ref() != Some(&phase_id) {
        return Err(HermeticProjectorError::WorkerStreamConflict);
    }
    let attempt = envelope
        .attempt()
        .ok_or(HermeticProjectorError::WorkerStreamConflict)?;
    let binding = CanonicalWorkerAttempt {
        phase_id: phase_id.clone(),
        worker_id: envelope.identity().worker_id().to_owned(),
        attempt,
    };
    if transition_id != worker_transition_id(state.mission_id(), &binding, kind) {
        return Err(HermeticProjectorError::TransitionIdentityConflict);
    }
    ensure_worker_envelope_binding(&envelope, state.mission_id(), &binding, kind)?;
    if let WorkerEventKind::Spawned = kind {
        let WorkerEventPayload::Spawned(spawned) = envelope.payload() else {
            return Err(HermeticProjectorError::WorkerStreamConflict);
        };
        let persona = spawned
            .persona()
            .filter(|persona| !persona.is_empty())
            .ok_or(HermeticProjectorError::WorkerStreamConflict)?;
        let workspace =
            WorkspaceAuthority::admit_production(Arc::clone(boundary), state.mission_id().clone())?;
        let phase_worker = match workspace.phase_worker_binding(
            persona,
            &phase_id,
            std::path::Path::new(spawned.directory()),
        ) {
            Ok(binding) => binding,
            Err(WorkspaceError::ProcessWorkingRootMismatch) => {
                return Err(HermeticProjectorError::WorkerStreamConflict);
            }
            Err(error) => return Err(error.into()),
        };
        if phase_worker.worker_id().as_str() != binding.worker_id
            || spawned.runtime().is_none()
            || spawned.effort_level().is_none()
        {
            return Err(HermeticProjectorError::WorkerStreamConflict);
        }
    }
    if streams.get(&binding).is_none() {
        ensure_new_worker_attempt_is_ordered(streams, &binding)?;
    }
    let next = match (streams.get(&binding).copied(), kind) {
        (None, WorkerEventKind::Spawned) => CanonicalWorkerStream::Spawned,
        (Some(CanonicalWorkerStream::Spawned), WorkerEventKind::Completed) => {
            CanonicalWorkerStream::Completed
        }
        (Some(CanonicalWorkerStream::Spawned), WorkerEventKind::Failed) => {
            CanonicalWorkerStream::Failed
        }
        _ => return Err(HermeticProjectorError::WorkerStreamConflict),
    };
    Ok(Some((binding, next)))
}

fn build_pristine_state(
    mission_id: &MissionId,
    plan: &CheckpointPlan,
) -> Result<MissionState, HermeticProjectorError> {
    let definitions = plan
        .phases
        .iter()
        .map(|phase| {
            let id = PhaseId::new(phase.id.clone())?;
            let dependencies = phase
                .dependencies
                .iter()
                .cloned()
                .map(PhaseId::new)
                .collect::<Result<Vec<_>, CoreError>>()?;
            Ok(PhaseDefinition { id, dependencies })
        })
        .collect::<Result<Vec<_>, CoreError>>()?;
    Ok(MissionState::new(mission_id.clone(), definitions)?)
}

fn admission_transition_id(mission_id: &MissionId) -> String {
    format!("hermetic-admission:{}", mission_id.as_str())
}

fn lifecycle_transition_id(
    mission_id: &MissionId,
    phase_id: Option<&PhaseId>,
    kind: &str,
) -> String {
    format!(
        "hermetic-lifecycle:{}:{}:{kind}",
        mission_id.as_str(),
        phase_id.map_or("mission", PhaseId::as_str),
    )
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Maps a reducer transition onto its stable Go-compatible event type,
/// mirroring `lifecycle.rs`'s `event_record` mapping exactly (they must
/// never drift apart, since both writers project onto the same event-type
/// vocabulary).
fn event_type_str(transition: &ReducerTransition) -> &'static str {
    match transition {
        ReducerTransition::MissionStarted => "mission.started",
        ReducerTransition::MissionCompleted => "mission.completed",
        ReducerTransition::MissionFailed => "mission.failed",
        ReducerTransition::MissionCancelled { .. } => "mission.cancelled",
        ReducerTransition::PhaseStarted => "phase.started",
        ReducerTransition::PhaseCompleted => "phase.completed",
        ReducerTransition::PhaseFailed { .. } => "phase.failed",
        ReducerTransition::PhaseSkipped { .. } => "phase.skipped",
        ReducerTransition::PhaseRetrying | ReducerTransition::Unknown { .. } => {
            unreachable!("caller rejects unsupported transitions before this is reached")
        }
    }
}

fn transition_reason(transition: &ReducerTransition) -> Option<&str> {
    match transition {
        ReducerTransition::MissionCancelled { reason }
        | ReducerTransition::PhaseSkipped { reason } => Some(reason.as_str()),
        ReducerTransition::PhaseFailed { error } => Some(error.as_str()),
        _ => None,
    }
}

fn parse_transition_kind(
    kind: &str,
    reason: Option<String>,
) -> Result<ReducerTransition, HermeticProjectorError> {
    match (kind, reason) {
        ("mission.started", None) => Ok(ReducerTransition::MissionStarted),
        ("mission.completed", None) => Ok(ReducerTransition::MissionCompleted),
        ("mission.failed", None) => Ok(ReducerTransition::MissionFailed),
        ("mission.cancelled", Some(reason)) => Ok(ReducerTransition::MissionCancelled { reason }),
        ("phase.started", None) => Ok(ReducerTransition::PhaseStarted),
        ("phase.completed", None) => Ok(ReducerTransition::PhaseCompleted),
        ("phase.failed", Some(error)) => Ok(ReducerTransition::PhaseFailed { error }),
        ("phase.skipped", Some(reason)) => Ok(ReducerTransition::PhaseSkipped { reason }),
        (
            "mission.started" | "mission.completed" | "mission.failed" | "mission.cancelled"
            | "phase.started" | "phase.completed" | "phase.failed" | "phase.skipped",
            _,
        ) => Err(HermeticProjectorError::CorruptJournalPayload),
        (other, _) => Err(HermeticProjectorError::UnrecognizedTransitionKind(
            other.to_owned(),
        )),
    }
}

/// Builds this projector's canonical journal payload shape:
/// `{"phase_id": <string, omitted if absent>, "data": <object-or-null>,
/// "reason": <string, omitted if absent>}`. Every other top-level field is
/// reserved; `RuntimeStore`'s own event-projection-binding validation parses
/// this same shape (phase_id/worker_id/data are extracted explicitly, and
/// everything else — here, only `reason` — becomes `EventRecord::extra`), so
/// this function and [`build_event_record`] must stay in lock-step.
fn encode_payload(
    phase_id: Option<&PhaseId>,
    data: &Value,
    reason: Option<&str>,
) -> Result<Value, HermeticProjectorError> {
    if !matches!(data, Value::Null | Value::Object(_)) {
        return Err(HermeticProjectorError::InvalidEventData);
    }
    let mut object = serde_json::Map::new();
    if let Some(phase_id) = phase_id {
        object.insert(
            "phase_id".to_owned(),
            Value::String(phase_id.as_str().to_owned()),
        );
    }
    if !matches!(data, Value::Null) {
        object.insert("data".to_owned(), data.clone());
    }
    if let Some(reason) = reason {
        object.insert("reason".to_owned(), Value::String(reason.to_owned()));
    }
    Ok(Value::Object(object))
}

struct DecodedProjectionPayload {
    phase_id: Option<PhaseId>,
    worker_id: Option<String>,
    data: Value,
    reason: Option<String>,
}

/// Inverse of [`encode_payload`] and [`encode_worker_payload`], used only for recovery.
fn decode_payload(payload_json: &str) -> Result<DecodedProjectionPayload, HermeticProjectorError> {
    let value: Value = serde_json::from_str(payload_json)
        .map_err(|_| HermeticProjectorError::CorruptJournalPayload)?;
    let Value::Object(mut object) = value else {
        return Err(HermeticProjectorError::CorruptJournalPayload);
    };
    let phase_id = match object.remove("phase_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() => {
            Some(PhaseId::new(value).map_err(HermeticProjectorError::from)?)
        }
        Some(_) => return Err(HermeticProjectorError::CorruptJournalPayload),
    };
    let worker_id = match object.remove("worker_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() => {
            WorkerId::new(value.clone()).map_err(HermeticProjectorError::from)?;
            Some(value)
        }
        Some(_) => return Err(HermeticProjectorError::CorruptJournalPayload),
    };
    let data = match object.remove("data") {
        None => Value::Null,
        Some(value @ (Value::Null | Value::Object(_))) => value,
        Some(_) => return Err(HermeticProjectorError::CorruptJournalPayload),
    };
    let reason = match object.remove("reason") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() => Some(value),
        Some(_) => return Err(HermeticProjectorError::CorruptJournalPayload),
    };
    if !object.is_empty() {
        return Err(HermeticProjectorError::CorruptJournalPayload);
    }
    Ok(DecodedProjectionPayload {
        phase_id,
        worker_id,
        data,
        reason,
    })
}

/// Derives one exact [`EventRecord`] purely from journal truth: event type
/// from the transition kind, timestamp from committed time, mission from the
/// journal mission, phase/data/reason from the canonical payload, and
/// id/sequence from the sealed recipe. Receipts attest a projection of this
/// record; they never choose it.
fn build_event_record(
    mission_id: &MissionId,
    event_type: &str,
    committed_at_utc: &str,
    recipe: &EventProjectionRecipe,
    phase_id: Option<&PhaseId>,
    data: &Value,
    reason: Option<&str>,
) -> Result<EventRecord, HermeticProjectorError> {
    build_projection_event_record(
        mission_id,
        event_type,
        committed_at_utc,
        recipe,
        phase_id,
        None,
        data,
        reason,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "canonical journal-to-event mapper; every argument is independently sealed"
)]
fn build_projection_event_record(
    mission_id: &MissionId,
    event_type: &str,
    committed_at_utc: &str,
    recipe: &EventProjectionRecipe,
    phase_id: Option<&PhaseId>,
    worker_id: Option<&str>,
    data: &Value,
    reason: Option<&str>,
) -> Result<EventRecord, HermeticProjectorError> {
    let data = match data {
        Value::Null => None,
        Value::Object(values) => Some(
            values
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<EventJsonMap>(),
        ),
        _ => return Err(HermeticProjectorError::InvalidEventData),
    };
    let mut extra = BTreeMap::new();
    if let Some(reason) = reason {
        extra.insert("reason".to_owned(), Value::String(reason.to_owned()));
    }
    Ok(EventRecord {
        id: recipe.event_id().to_owned(),
        event_type: event_type.to_owned(),
        timestamp: committed_at_utc.to_owned(),
        sequence: recipe.public_sequence(),
        mission_id: mission_id.as_str().to_owned(),
        phase_id: phase_id.map(|phase_id| phase_id.as_str().to_owned()),
        worker_id: worker_id.map(str::to_owned),
        data,
        extra: EventJsonMap::from(extra),
    })
}

/// Attempts to upgrade from no checkpoint writer to a recovered one for a
/// workspace `into_production_projection_writer` has already declined
/// (non-pristine). `expectation` proves the caller independently derived a
/// canonical checkpoint fixed point from journal-reduced state — never a
/// disk read or a guess — so that a successful upgrade is a genuine proof,
/// not an echo. See
/// [`WorkspaceAuthority::into_production_projection_writer_recovered`]'s
/// doc comment for the full authority model.
///
/// Any failure to acquire (divergent bytes, wrong shape, or foreign fixture
/// history) becomes this composition's one terminal, pre-existing error for
/// an unrecoverable checkpoint —
/// [`HermeticProjectorError::ProjectionRecoveryUnavailable`] — rather than a
/// raw [`WorkspaceError`] passthrough: from this composition's point of
/// view, every one of those cases means the same thing (this checkpoint
/// cannot be safely resumed), and callers already match on that one variant.
fn try_recover_checkpoint_writer(
    boundary: &Arc<ProductionBoundary>,
    mission_id: &MissionId,
    expectation: JournalCheckpointExpectation,
) -> Result<ProductionProjectionWriter, HermeticProjectorError> {
    let workspace = WorkspaceAuthority::admit_production(Arc::clone(boundary), mission_id.clone())?;
    workspace
        .into_production_projection_writer_recovered(expectation)
        .map_err(|_| {
            HermeticProjectorError::ProjectionRecoveryUnavailable(
                CompatibilityProjection::Checkpoint,
            )
        })
}

/// Verifies (and, if a writer is available, mints) the admission record's
/// Workspace/Checkpoint receipts. A missing receipt with no writer available
/// is only tolerated when the durable checkpoint already matches the
/// pristine target exactly (a crossed crash boundary); otherwise this fails
/// closed, since admission's own target state is always pristine and a
/// writer should always have been obtainable for it.
#[expect(
    clippy::too_many_arguments,
    reason = "internal recovery helper; see caller"
)]
fn ensure_admission_receipts(
    store: &mut RuntimeStore,
    record: &ProjectionRecoveryRecord,
    writer: Option<&ProductionProjectionWriter>,
    boundary: &Arc<ProductionBoundary>,
    mission_id: &MissionId,
    mission_markdown: &[u8],
    plan: &CheckpointPlan,
    checkpoint_bytes: &[u8],
) -> Result<(), HermeticProjectorError> {
    if record.missing_projections().is_empty() {
        return Ok(());
    }
    let applied_at_utc = record.committed_at_utc().to_owned();
    match writer {
        Some(writer) => {
            writer.verify_exact_base(mission_markdown, plan, checkpoint_bytes)?;
        }
        None => {
            let workspace =
                WorkspaceAuthority::admit_production(Arc::clone(boundary), mission_id.clone())?;
            let current_checkpoint = workspace.verified_lifecycle_checkpoint_bytes()?;
            if current_checkpoint != checkpoint_bytes {
                return Err(HermeticProjectorError::ProjectionRecoveryUnavailable(
                    CompatibilityProjection::Checkpoint,
                ));
            }
        }
    }
    for projection in record.missing_projections() {
        let receipt = ProjectionReceipt::compatibility(
            record.journal_sequence(),
            *projection,
            applied_at_utc.clone(),
        )?;
        store.record_projection(&receipt)?;
    }
    Ok(())
}

/// Cross-checks one already-receipted journal record's reconstructed event
/// against the actual bytes already durable in the canonical event log,
/// rather than trusting the receipt. Fails closed on any divergence:
/// missing entry, wrong content, or wrong sequence placement.
fn verify_already_published_event(
    event_log: &CanonicalEventLog,
    recipe: &EventProjectionRecipe,
    expected_record: &EventRecord,
) -> Result<(), HermeticProjectorError> {
    let index = usize::try_from(recipe.public_sequence().saturating_sub(1))
        .map_err(|_| HermeticProjectorError::MissingEventRecipe)?;
    let published = event_log
        .events()
        .get(index)
        .ok_or(HermeticProjectorError::MissingEventRecipe)?;
    if published.record != *expected_record {
        return Err(HermeticProjectorError::ProjectionRecoveryUnavailable(
            CompatibilityProjection::EventLog,
        ));
    }
    Ok(())
}

/// Publishes (or confirms already-published, idempotently) one lifecycle
/// transition's event and checkpoint, and records both receipts.
#[expect(
    clippy::too_many_arguments,
    reason = "internal recovery/apply helper; see callers"
)]
fn publish_and_receipt(
    store: &mut RuntimeStore,
    event_log: &mut CanonicalEventLog,
    writer: &mut Option<ProductionProjectionWriter>,
    checkpoint_disposition: &mut Option<CheckpointReconciliationDisposition>,
    boundary: &Arc<ProductionBoundary>,
    mission_id: &MissionId,
    journal_sequence: i64,
    recipe: &EventProjectionRecipe,
    record: &EventRecord,
    prior_checkpoint: &CheckpointProjection,
    prior_state: &MissionState,
    prior_checkpoint_bytes: &[u8],
    target_checkpoint_bytes: &[u8],
    applied_at_utc: &str,
    r0_crash_point: Option<R0ProjectionCrashPoint>,
) -> Result<(), HermeticProjectorError> {
    // Bytes strictly before this recipe's own sequence — never "whatever is
    // on disk right now", which would already include this exact event on a
    // same-process retry and double-append it. Filtering by sequence is what
    // makes an exact retry (this event already published) land on
    // `publish_exact_next_line`'s crossed-crash-boundary branch instead of a
    // spurious duplicate-append attempt.
    let mut prior_event_bytes = Vec::new();
    for existing in event_log.events() {
        if existing.record.sequence >= recipe.public_sequence() {
            break;
        }
        prior_event_bytes.extend_from_slice(&existing.raw_line);
    }
    let content = encode_current_event(record)?;
    let separator_required = !prior_event_bytes.is_empty() && !prior_event_bytes.ends_with(b"\n");
    let mut next_line = Vec::with_capacity(content.len() + 2);
    if separator_required {
        next_line.push(b'\n');
    }
    next_line.extend_from_slice(&content);
    next_line.push(b'\n');
    let verified_event = event_log.publish_exact_next_line(&prior_event_bytes, &next_line)?;
    r0_crash_if(r0_crash_point, R0ProjectionCrashPoint::EventPublish)?;
    let mut event_jsonl = verified_event.line_bytes().to_vec();
    event_jsonl.push(b'\n');
    let event_receipt =
        ProjectionReceipt::event_log(journal_sequence, event_jsonl, applied_at_utc.to_owned())?;
    store.record_projection(&event_receipt)?;

    match writer.as_ref() {
        Some(existing) => {
            let verified =
                existing.reconcile_checkpoint(prior_checkpoint_bytes, target_checkpoint_bytes)?;
            *checkpoint_disposition = Some(verified.disposition());
            r0_crash_if(r0_crash_point, R0ProjectionCrashPoint::CheckpointPublish)?;
            let checkpoint_receipt = ProjectionReceipt::compatibility(
                journal_sequence,
                CompatibilityProjection::Checkpoint,
                applied_at_utc.to_owned(),
            )?;
            store.record_projection(&checkpoint_receipt)?;
        }
        None => {
            let workspace =
                WorkspaceAuthority::admit_production(Arc::clone(boundary), mission_id.clone())?;
            let current = workspace.verified_lifecycle_checkpoint_bytes()?;
            if current == target_checkpoint_bytes {
                // Crossed crash boundary: the write already landed durably
                // before an earlier crash, only its receipt was lost. No
                // writer is needed (or attempted) to confirm that — this is
                // exactly the case `crash_after_checkpoint_rename_before_receipt_recovers_via_reopen`
                // exercises, and it must keep working without ever touching
                // the recovery constructor.
                *checkpoint_disposition = Some(CheckpointReconciliationDisposition::AlreadyTarget);
                r0_crash_if(r0_crash_point, R0ProjectionCrashPoint::CheckpointPublish)?;
                let checkpoint_receipt = ProjectionReceipt::compatibility(
                    journal_sequence,
                    CompatibilityProjection::Checkpoint,
                    applied_at_utc.to_owned(),
                )?;
                store.record_projection(&checkpoint_receipt)?;
            } else {
                // A real write is still needed. Lazily upgrade to a
                // recovered writer, proving `prior_checkpoint_bytes` — this
                // record's own journal-derived expectation for whatever must
                // currently be durable, now that "already at target" is
                // ruled out above — against disk before minting one. Once
                // acquired, the writer is stored back into the caller's slot
                // so later records in the same replay (or a later live
                // `apply_transition` call) reuse it instead of re-deriving
                // and re-verifying it from scratch each time.
                //
                // If disk genuinely diverges from both `target` (just ruled
                // out) and `prior` (checked here), the recovery constructor
                // itself rejects it and this maps to the same terminal
                // `ProjectionRecoveryUnavailable` this composition has
                // always used for an unrecoverable checkpoint — the gap
                // closing does not adopt a state the journal replay cannot
                // account for.
                let expectation = journal_checkpoint_expectation(prior_checkpoint, prior_state)?;
                let recovered = try_recover_checkpoint_writer(boundary, mission_id, expectation)?;
                let verified = recovered
                    .reconcile_checkpoint(prior_checkpoint_bytes, target_checkpoint_bytes)?;
                *checkpoint_disposition = Some(verified.disposition());
                r0_crash_if(r0_crash_point, R0ProjectionCrashPoint::CheckpointPublish)?;
                let checkpoint_receipt = ProjectionReceipt::compatibility(
                    journal_sequence,
                    CompatibilityProjection::Checkpoint,
                    applied_at_utc.to_owned(),
                )?;
                store.record_projection(&checkpoint_receipt)?;
                *writer = Some(recovered);
            }
        }
    }
    Ok(())
}

/// Journal-first crash cuts exposed only to the hermetic R0 restart harness.
///
/// The caller must supply an already-admitted fixture authority. There is no
/// raw-path constructor, live-home enrollment, provider, or arbitrary
/// transition selector on this surface.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum R0JournalCrashCell {
    /// Mission-start journal commit before its event/checkpoint projections.
    MissionStartBeforeProjection,
    /// Phase-start journal commit before its event/checkpoint projections.
    PhaseStartBeforeProjection,
    /// Terminal event append before either projection receipt or checkpoint publication.
    EventAppendBeforeCheckpoint,
    /// Checkpoint rename and directory sync before its projection receipt.
    CheckpointFsyncBeforeReceipt,
}

impl R0JournalCrashCell {
    const fn transition(self) -> ReducerTransition {
        match self {
            Self::MissionStartBeforeProjection => ReducerTransition::MissionStarted,
            Self::PhaseStartBeforeProjection => ReducerTransition::PhaseStarted,
            Self::EventAppendBeforeCheckpoint | Self::CheckpointFsyncBeforeReceipt => {
                ReducerTransition::PhaseCompleted
            }
        }
    }

    const fn timestamp(self) -> &'static str {
        match self {
            Self::MissionStartBeforeProjection => "2026-07-31T00:01:01Z",
            Self::PhaseStartBeforeProjection => "2026-07-31T00:01:02Z",
            Self::EventAppendBeforeCheckpoint | Self::CheckpointFsyncBeforeReceipt => {
                "2026-07-31T00:01:03Z"
            }
        }
    }
}

/// Opaque owner retaining the unclosed journal writer at an R0 crash cut.
///
/// A successful test never drops this value: the parent sends `SIGKILL` to
/// the child process after receiving its durability acknowledgement.
#[doc(hidden)]
pub struct R0JournalCrashGuard {
    _authority: FreshFixtureAuthority,
    _projector: HermeticCompatibilityProjector,
}

impl fmt::Debug for R0JournalCrashGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("R0JournalCrashGuard")
            .field("kind", &"fixture-bound-journal-crash-cut")
            .finish()
    }
}

/// Stable observations returned after replaying one journal-first R0 cut.
#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct R0JournalRecoveryReport {
    event_types: Vec<String>,
    event_bytes: Vec<u8>,
    checkpoint_bytes: Vec<u8>,
    mission_status: String,
    phase_status: String,
    journal_record_count: usize,
    acknowledged_record_count: usize,
    projection_receipt_count: usize,
    incomplete_record_count: usize,
    target_transition_count: usize,
    recovery_checkpoint_disposition: Option<CheckpointReconciliationDisposition>,
}

impl R0JournalRecoveryReport {
    /// Canonical event types after recovery, in durable sequence order.
    #[must_use]
    pub fn event_types(&self) -> &[String] {
        &self.event_types
    }

    /// Exact recovered event-log bytes.
    #[must_use]
    pub fn event_bytes(&self) -> &[u8] {
        &self.event_bytes
    }

    /// Exact recovered checkpoint bytes.
    #[must_use]
    pub fn checkpoint_bytes(&self) -> &[u8] {
        &self.checkpoint_bytes
    }

    /// Go-compatible mission status after recovery.
    #[must_use]
    pub fn mission_status(&self) -> &str {
        &self.mission_status
    }

    /// Go-compatible status of the fixture's sole phase after recovery.
    #[must_use]
    pub fn phase_status(&self) -> &str {
        &self.phase_status
    }

    /// Exact number of retained journal records for the fixture mission.
    #[must_use]
    pub const fn journal_record_count(&self) -> usize {
        self.journal_record_count
    }

    /// Number of journal records carrying their durable command acknowledgement.
    #[must_use]
    pub const fn acknowledged_record_count(&self) -> usize {
        self.acknowledged_record_count
    }

    /// Total exact projection receipts retained across the fixture mission.
    #[must_use]
    pub const fn projection_receipt_count(&self) -> usize {
        self.projection_receipt_count
    }

    /// Number of records still missing a projection or acknowledgement.
    #[must_use]
    pub const fn incomplete_record_count(&self) -> usize {
        self.incomplete_record_count
    }

    /// Number of rows for the exact transition whose crash cut was exercised.
    #[must_use]
    pub const fn target_transition_count(&self) -> usize {
        self.target_transition_count
    }

    /// Checkpoint action taken while replaying the incomplete target record.
    ///
    /// `None` means the target record was already fully receipted when this
    /// recovery process opened it.
    #[must_use]
    pub const fn recovery_checkpoint_disposition(
        &self,
    ) -> Option<CheckpointReconciliationDisposition> {
        self.recovery_checkpoint_disposition
    }
}

/// Stable observations from the ordinary Cell 2 restart composition.
///
/// Unlike [`R0JournalRecoveryReport`], this report covers the complete
/// phase-start continuation: replay of the interrupted `phase.started`,
/// selection of the exact durable worker action, and canonical projection of
/// its `worker.spawned` plus terminal event.
#[doc(hidden)]
#[cfg(unix)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct R0PhaseStartContinuationReport {
    event_types: Vec<String>,
    event_bytes: Vec<u8>,
    checkpoint_bytes: Vec<u8>,
    mission_status: String,
    phase_status: String,
    journal_record_count: usize,
    acknowledged_record_count: usize,
    projection_receipt_count: usize,
    incomplete_record_count: usize,
    phase_start_transition_count: usize,
    recovery_checkpoint_disposition: Option<CheckpointReconciliationDisposition>,
    worker_id: String,
    attempt: u32,
    terminal_event_type: String,
    spawned_at_utc: String,
    terminal_at_utc: String,
    durable_committed_at_utc: String,
    projection_replayed: bool,
    durable_attempt: R0DurableAttemptReport,
}

#[cfg(unix)]
impl R0PhaseStartContinuationReport {
    /// Canonical event types after the complete continuation.
    #[must_use]
    pub fn event_types(&self) -> &[String] {
        &self.event_types
    }

    /// Exact canonical event-log bytes after continuation.
    #[must_use]
    pub fn event_bytes(&self) -> &[u8] {
        &self.event_bytes
    }

    /// Exact checkpoint bytes after continuation.
    #[must_use]
    pub fn checkpoint_bytes(&self) -> &[u8] {
        &self.checkpoint_bytes
    }

    /// Go-compatible mission status after worker projection.
    #[must_use]
    pub fn mission_status(&self) -> &str {
        &self.mission_status
    }

    /// Go-compatible phase status after worker projection.
    #[must_use]
    pub fn phase_status(&self) -> &str {
        &self.phase_status
    }

    /// Number of compatibility journal records for the fixture mission.
    #[must_use]
    pub const fn journal_record_count(&self) -> usize {
        self.journal_record_count
    }

    /// Number of journal records carrying their command acknowledgement.
    #[must_use]
    pub const fn acknowledged_record_count(&self) -> usize {
        self.acknowledged_record_count
    }

    /// Total number of exact compatibility projection receipts.
    #[must_use]
    pub const fn projection_receipt_count(&self) -> usize {
        self.projection_receipt_count
    }

    /// Number of journal records still missing acknowledgement or projection.
    #[must_use]
    pub const fn incomplete_record_count(&self) -> usize {
        self.incomplete_record_count
    }

    /// Number of retained `phase.started` transitions.
    #[must_use]
    pub const fn phase_start_transition_count(&self) -> usize {
        self.phase_start_transition_count
    }

    /// Checkpoint action taken while replaying the interrupted phase start.
    #[must_use]
    pub const fn recovery_checkpoint_disposition(
        &self,
    ) -> Option<CheckpointReconciliationDisposition> {
        self.recovery_checkpoint_disposition
    }

    /// Canonical worker identity derived from the sealed request.
    #[must_use]
    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    /// Exact logical worker attempt projected by the continuation.
    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Canonical terminal worker event type.
    #[must_use]
    pub fn terminal_event_type(&self) -> &str {
        &self.terminal_event_type
    }

    /// Timestamp retained on the canonical spawn event.
    #[must_use]
    pub fn spawned_at_utc(&self) -> &str {
        &self.spawned_at_utc
    }

    /// Timestamp retained on the canonical terminal event.
    #[must_use]
    pub fn terminal_at_utc(&self) -> &str {
        &self.terminal_at_utc
    }

    /// Durable terminal-observation timestamp that authorized projection.
    #[must_use]
    pub fn durable_committed_at_utc(&self) -> &str {
        &self.durable_committed_at_utc
    }

    /// Whether both canonical worker records were exact durable replays.
    #[must_use]
    pub const fn projection_replayed(&self) -> bool {
        self.projection_replayed
    }

    /// Stable private-ledger observations for the exact worker attempt.
    #[must_use]
    pub const fn durable_attempt(&self) -> &R0DurableAttemptReport {
        &self.durable_attempt
    }
}

/// Failures from the capability-bound R0 journal crash fixture.
#[doc(hidden)]
#[derive(Debug, Error)]
#[error("R0 journal crash fixture failed during {stage}: {detail}")]
pub struct R0JournalCrashError {
    stage: &'static str,
    detail: String,
}

fn r0_journal_error(stage: &'static str, error: impl fmt::Display) -> R0JournalCrashError {
    R0JournalCrashError {
        stage,
        detail: error.to_string(),
    }
}

const R0_JOURNAL_MISSION: &str = "r0-journal-restart";
const R0_JOURNAL_PHASE: &str = "verify";
const R0_JOURNAL_ADMISSION_AT: &str = "2026-07-31T00:01:00Z";

fn r0_journal_seed() -> MissionSeed {
    MissionSeed {
        mission_markdown: b"r0 journal restart fixture\n".to_vec(),
        plan: CheckpointPlan {
            id: "r0-journal-restart-plan".to_owned(),
            phases: vec![CheckpointPhase {
                id: R0_JOURNAL_PHASE.to_owned(),
                ..CheckpointPhase::default()
            }],
            ..CheckpointPlan::default()
        },
    }
}

fn r0_fixture_boundary(
    authority: &FreshFixtureAuthority,
) -> Result<Arc<ProductionBoundary>, R0JournalCrashError> {
    authority
        .boundary
        .verify()
        .map_err(|error| r0_journal_error("verify fixture authority", error))?;
    let boundary = ProductionBoundary::from_fixture_authority(authority)
        .map(Arc::new)
        .map_err(|error| r0_journal_error("bind fixture projection boundary", error))?;
    authority
        .boundary
        .verify()
        .map_err(|error| r0_journal_error("reverify fixture authority", error))?;
    Ok(boundary)
}

fn r0_open_prerequisite_projector(
    boundary: Arc<ProductionBoundary>,
    cell: R0JournalCrashCell,
) -> Result<HermeticCompatibilityProjector, R0JournalCrashError> {
    let mission = MissionId::new(R0_JOURNAL_MISSION)
        .map_err(|error| r0_journal_error("build mission identity", error))?;
    let mut projector = HermeticCompatibilityProjector::open(
        boundary,
        mission,
        r0_journal_seed(),
        R0_JOURNAL_ADMISSION_AT.to_owned(),
    )
    .map_err(|error| r0_journal_error("open compatibility projector", error))?;
    if cell != R0JournalCrashCell::MissionStartBeforeProjection {
        projector
            .apply_transition(
                None,
                ReducerTransition::MissionStarted,
                Value::Null,
                None,
                "2026-07-31T00:01:01Z".to_owned(),
            )
            .map_err(|error| r0_journal_error("publish mission prerequisite", error))?;
    }
    if matches!(
        cell,
        R0JournalCrashCell::EventAppendBeforeCheckpoint
            | R0JournalCrashCell::CheckpointFsyncBeforeReceipt
    ) {
        projector
            .apply_transition(
                Some(
                    PhaseId::new(R0_JOURNAL_PHASE)
                        .map_err(|error| r0_journal_error("build phase identity", error))?,
                ),
                ReducerTransition::PhaseStarted,
                Value::Null,
                None,
                "2026-07-31T00:01:02Z".to_owned(),
            )
            .map_err(|error| r0_journal_error("publish phase prerequisite", error))?;
    }
    Ok(projector)
}

/// Establishes one exact journal/projection crash cut for the R0 harness.
///
/// The returned guard retains the SQLite writer and fixture authority. The
/// harness must acknowledge the durable cut and then kill the child process;
/// gracefully dropping this value is not completion evidence.
#[doc(hidden)]
pub fn prepare_r0_journal_crash(
    authority: FreshFixtureAuthority,
    cell: R0JournalCrashCell,
) -> Result<R0JournalCrashGuard, R0JournalCrashError> {
    prepare_r0_journal_crash_via_projector(authority, cell)
}

fn prepare_r0_journal_crash_via_projector(
    authority: FreshFixtureAuthority,
    cell: R0JournalCrashCell,
) -> Result<R0JournalCrashGuard, R0JournalCrashError> {
    let boundary = r0_fixture_boundary(&authority)?;
    let mut projector = r0_open_prerequisite_projector(Arc::clone(&boundary), cell)?;
    let crash_point = match cell {
        R0JournalCrashCell::MissionStartBeforeProjection
        | R0JournalCrashCell::PhaseStartBeforeProjection => R0ProjectionCrashPoint::JournalCommit,
        R0JournalCrashCell::EventAppendBeforeCheckpoint => R0ProjectionCrashPoint::EventPublish,
        R0JournalCrashCell::CheckpointFsyncBeforeReceipt => {
            R0ProjectionCrashPoint::CheckpointPublish
        }
    };
    projector.last_checkpoint_disposition = None;
    projector.r0_crash_point = Some(crash_point);
    let phase = match cell {
        R0JournalCrashCell::MissionStartBeforeProjection => None,
        R0JournalCrashCell::PhaseStartBeforeProjection
        | R0JournalCrashCell::EventAppendBeforeCheckpoint
        | R0JournalCrashCell::CheckpointFsyncBeforeReceipt => Some(
            PhaseId::new(R0_JOURNAL_PHASE)
                .map_err(|error| r0_journal_error("build phase identity", error))?,
        ),
    };
    let verification = matches!(
        cell,
        R0JournalCrashCell::EventAppendBeforeCheckpoint
            | R0JournalCrashCell::CheckpointFsyncBeforeReceipt
    )
    .then(|| {
        decide_verification(
            VerificationOutcome::Classified(VerificationClass::Pass),
            VerificationMode::Block,
        )
    });
    let result = projector.apply_transition(
        phase,
        cell.transition(),
        Value::Null,
        verification,
        cell.timestamp().to_owned(),
    );
    match result {
        Err(HermeticProjectorError::R0InjectedCrashCut(found)) if found == crash_point.label() => {}
        other => {
            return Err(r0_journal_error(
                "reach production projection cut",
                format!("unexpected projector result: {other:?}"),
            ));
        }
    }
    let expected_checkpoint_disposition = (crash_point
        == R0ProjectionCrashPoint::CheckpointPublish)
        .then_some(CheckpointReconciliationDisposition::Published);
    if projector.last_checkpoint_disposition != expected_checkpoint_disposition {
        return Err(r0_journal_error(
            "inspect checkpoint cut disposition",
            format!(
                "expected {expected_checkpoint_disposition:?}, found {:?}",
                projector.last_checkpoint_disposition
            ),
        ));
    }

    let mission = MissionId::new(R0_JOURNAL_MISSION)
        .map_err(|error| r0_journal_error("build mission identity", error))?;
    let bounds = ProjectionRecoveryBounds::new(32, 256 * 1024)
        .map_err(|error| r0_journal_error("build recovery bounds", error))?;
    let snapshot = projector
        .store
        .projection_recovery_snapshot(&mission, &bounds)
        .map_err(|error| r0_journal_error("inspect production cut", error))?;
    let target_kind = event_type_str(&cell.transition());
    let target_records = snapshot
        .records()
        .iter()
        .filter(|record| record.transition_kind() == target_kind)
        .collect::<Vec<_>>();
    let [record] = target_records.as_slice() else {
        return Err(r0_journal_error(
            "inspect production cut",
            format!(
                "expected one {target_kind} row, found {}",
                target_records.len()
            ),
        ));
    };
    let expected_missing = match crash_point {
        R0ProjectionCrashPoint::JournalCommit | R0ProjectionCrashPoint::EventPublish => [
            CompatibilityProjection::Checkpoint,
            CompatibilityProjection::EventLog,
        ]
        .as_slice(),
        R0ProjectionCrashPoint::CheckpointPublish => {
            [CompatibilityProjection::Checkpoint].as_slice()
        }
    };
    let expected_receipts = usize::from(crash_point == R0ProjectionCrashPoint::CheckpointPublish);
    let expected_published_events =
        usize::from(crash_point != R0ProjectionCrashPoint::JournalCommit);
    let published_events = projector
        .event_log
        .events()
        .iter()
        .filter(|event| event.record.event_type == target_kind)
        .count();
    if record.missing_projections() != expected_missing
        || record.present_receipts().len() != expected_receipts
        || record.acknowledgement().is_some()
        || published_events != expected_published_events
    {
        return Err(r0_journal_error(
            "inspect production cut",
            "production projector crossed or failed to reach the requested boundary",
        ));
    }

    let durable_checkpoint = WorkspaceAuthority::admit_production(Arc::clone(&boundary), mission)
        .and_then(|workspace| workspace.verified_lifecycle_checkpoint_bytes())
        .map_err(|error| r0_journal_error("inspect durable checkpoint cut", error))?;
    let checkpoint = orchestrator_core::decode_checkpoint(&durable_checkpoint)
        .map_err(|error| r0_journal_error("decode durable checkpoint cut", error))?
        .projection;
    let mission_status = checkpoint.status.clone();
    let phase_status = checkpoint
        .plan
        .and_then(|plan| plan.phases.into_iter().next())
        .map(|phase| phase.status)
        .ok_or_else(|| r0_journal_error("decode durable checkpoint cut", "phase is missing"))?;
    let expected_phase_status = match cell {
        R0JournalCrashCell::MissionStartBeforeProjection
        | R0JournalCrashCell::PhaseStartBeforeProjection => "pending",
        R0JournalCrashCell::EventAppendBeforeCheckpoint => "running",
        R0JournalCrashCell::CheckpointFsyncBeforeReceipt => "completed",
    };
    let expected_mission_status = match cell {
        R0JournalCrashCell::MissionStartBeforeProjection => "pending",
        R0JournalCrashCell::PhaseStartBeforeProjection
        | R0JournalCrashCell::EventAppendBeforeCheckpoint
        | R0JournalCrashCell::CheckpointFsyncBeforeReceipt => "in_progress",
    };
    if mission_status != expected_mission_status || phase_status != expected_phase_status {
        return Err(r0_journal_error(
            "inspect durable checkpoint cut",
            format!(
                "expected mission={expected_mission_status}, phase={expected_phase_status}; \
                 found mission={mission_status}, phase={phase_status}"
            ),
        ));
    }

    Ok(R0JournalCrashGuard {
        _authority: authority,
        _projector: projector,
    })
}

/// Opaque live recovery owner used by the Cell 2 continuation adapter.
///
/// Opening this value completes journal replay but deliberately retains both
/// the fixture capability and the compatibility projector's writer. The
/// legacy public report API below borrows it to build observations and then
/// closes it. On Unix, the Cell 2 continuation method selects run-or-replay
/// from canonical worker-stream state and projects only the provider module's
/// sealed durable-process evidence through the retained writer.
pub(crate) struct R0JournalRecoverySession {
    authority: FreshFixtureAuthority,
    cell: R0JournalCrashCell,
    projector: HermeticCompatibilityProjector,
}

pub(crate) fn open_r0_journal_recovery(
    authority: FreshFixtureAuthority,
    cell: R0JournalCrashCell,
) -> Result<R0JournalRecoverySession, R0JournalCrashError> {
    let boundary = r0_fixture_boundary(&authority)?;
    let mission = MissionId::new(R0_JOURNAL_MISSION)
        .map_err(|error| r0_journal_error("build mission identity", error))?;
    let projector = HermeticCompatibilityProjector::open(
        boundary,
        mission,
        r0_journal_seed(),
        R0_JOURNAL_ADMISSION_AT.to_owned(),
    )
    .map_err(|error| r0_journal_error("replay compatibility projector", error))?;
    Ok(R0JournalRecoverySession {
        authority,
        cell,
        projector,
    })
}

impl R0JournalRecoverySession {
    fn report(&mut self) -> Result<R0JournalRecoveryReport, R0JournalCrashError> {
        build_r0_journal_recovery_report(&mut self.projector, self.cell)
    }

    #[cfg(unix)]
    fn obtain_phase_start_worker(
        &mut self,
        helper_bytes: &[u8],
    ) -> Result<R0DurableWorkerProjection, R0JournalCrashError> {
        if self.cell != R0JournalCrashCell::PhaseStartBeforeProjection
            || self.projector.state().status() != MissionStatus::InProgress
            || sole_running_phase_id(self.projector.state())
                .is_none_or(|phase| phase.as_str() != R0_JOURNAL_PHASE)
        {
            return Err(r0_journal_error(
                "select phase-start continuation",
                "recovery does not retain the exact running Cell 2 phase",
            ));
        }

        // Any canonical worker prefix removes admission authority. Even a
        // malformed or foreign-looking prefix must take the ExistingOnly path
        // and fail closed during exact request binding; it must never cause a
        // new private ledger or process launch.
        let durable = if self.projector.canonical_worker_streams.is_empty() {
            run_or_replay_r0_phase_start_durable_attempt(&self.authority, helper_bytes)
        } else {
            inspect_existing_r0_phase_start_durable_attempt(&self.authority, helper_bytes)
        }
        .map_err(|error| r0_journal_error("obtain durable Cell 2 worker evidence", error))?;
        let evidence = durable.evidence();
        if evidence.decision().observed_termination() != evidence.outcome().termination() {
            return Err(r0_journal_error(
                "verify durable Cell 2 worker evidence",
                "terminal decision does not match the sealed process outcome",
            ));
        }
        Ok(durable)
    }

    #[cfg(unix)]
    fn project_phase_start_worker(
        &mut self,
        durable: R0DurableWorkerProjection,
    ) -> Result<R0PhaseStartContinuation, R0JournalCrashError> {
        let evidence = durable.evidence();
        let committed_at_utc = evidence.committed_at_utc().to_owned();
        let projection = self
            .projector
            .project_durable_worker_attempt(DurableWorkerProjectionInput::from_provider_evidence(
                evidence,
            ))
            .map_err(|error| r0_journal_error("project durable Cell 2 worker", error))?;
        Ok(R0PhaseStartContinuation {
            projection,
            durable_attempt: *durable.report(),
            committed_at_utc,
        })
    }

    #[cfg(unix)]
    fn continue_phase_start_worker(
        &mut self,
        helper_bytes: &[u8],
    ) -> Result<R0PhaseStartContinuation, R0JournalCrashError> {
        let durable = self.obtain_phase_start_worker(helper_bytes)?;
        self.project_phase_start_worker(durable)
    }

    fn close(self) -> Result<(), R0JournalCrashError> {
        self.projector
            .close()
            .map_err(|error| r0_journal_error("close recovered projector", error))
    }
}

/// Runs the ordinary Cell 2 continuation through durable process termination
/// and decision persistence, then exposes the exact pre-worker-projection cut
/// to a separate SIGKILL harness.
///
/// `notify` receives only a copy of the closed process-ledger summary. It must
/// make its parent-visible acknowledgement durable and then block. If it
/// returns, no worker projection is attempted and this function fails.
#[doc(hidden)]
#[cfg(unix)]
pub fn run_r0_phase_start_crash_until_worker_projection_barrier<Notify>(
    authority: FreshFixtureAuthority,
    helper_bytes: &[u8],
    notify: Notify,
) -> Result<(), R0JournalCrashError>
where
    Notify: FnOnce(R0DurableAttemptReport),
{
    let mut session =
        open_r0_journal_recovery(authority, R0JournalCrashCell::PhaseStartBeforeProjection)?;
    if session.projector.last_checkpoint_disposition
        != Some(CheckpointReconciliationDisposition::Published)
    {
        return Err(r0_journal_error(
            "verify Cell 2 phase-start recovery",
            "the interrupted phase-start checkpoint was not published exactly once",
        ));
    }
    let durable = session.obtain_phase_start_worker(helper_bytes)?;
    notify(*durable.report());
    Err(r0_journal_error(
        "hold Cell 2 worker projection barrier",
        "barrier callback returned before SIGKILL",
    ))
}

#[cfg(unix)]
struct R0PhaseStartContinuation {
    projection: CanonicalWorkerProjection,
    durable_attempt: R0DurableAttemptReport,
    committed_at_utc: String,
}

/// Reopens the interrupted phase start and continues its exact worker action
/// through the ordinary durable-process and canonical-projector composition.
///
/// A canonical worker prefix switches the process side to `ExistingOnly`
/// before any private target can be admitted. Consequently, a second fresh
/// reopen can only inspect the retained terminal evidence and replay the two
/// exact worker records; it cannot launch the helper again.
#[doc(hidden)]
#[cfg(unix)]
pub fn recover_r0_phase_start_continuation(
    authority: FreshFixtureAuthority,
    helper_bytes: &[u8],
) -> Result<R0PhaseStartContinuationReport, R0JournalCrashError> {
    let mut session =
        open_r0_journal_recovery(authority, R0JournalCrashCell::PhaseStartBeforeProjection)?;
    let recovery_checkpoint_disposition = session.projector.last_checkpoint_disposition;
    let continuation = session.continue_phase_start_worker(helper_bytes)?;
    let report = build_r0_phase_start_continuation_report(
        &mut session.projector,
        continuation,
        recovery_checkpoint_disposition,
    )?;
    session.close()?;
    Ok(report)
}

/// Reopens one killed journal-first fixture and completes its exact projections.
#[doc(hidden)]
pub fn recover_r0_journal_crash(
    authority: FreshFixtureAuthority,
    cell: R0JournalCrashCell,
) -> Result<R0JournalRecoveryReport, R0JournalCrashError> {
    let mut session = open_r0_journal_recovery(authority, cell)?;
    let report = session.report()?;
    session.close()?;
    Ok(report)
}

#[cfg(unix)]
fn build_r0_phase_start_continuation_report(
    projector: &mut HermeticCompatibilityProjector,
    continuation: R0PhaseStartContinuation,
    recovery_checkpoint_disposition: Option<CheckpointReconciliationDisposition>,
) -> Result<R0PhaseStartContinuationReport, R0JournalCrashError> {
    let mission = MissionId::new(R0_JOURNAL_MISSION)
        .map_err(|error| r0_journal_error("build Cell 2 mission identity", error))?;
    let checkpoint_bytes = projector.checkpoint_bytes().to_vec();
    let checkpoint = orchestrator_core::decode_checkpoint(&checkpoint_bytes)
        .map_err(|error| r0_journal_error("decode Cell 2 checkpoint", error))?
        .projection;
    let phase_status = checkpoint
        .plan
        .as_ref()
        .and_then(|plan| plan.phases.first())
        .map(|phase| phase.status.clone())
        .ok_or_else(|| r0_journal_error("decode Cell 2 checkpoint", "phase is missing"))?;

    let spawned = continuation.projection.spawned();
    let terminal = continuation.projection.terminal();
    let attempt = spawned
        .attempt()
        .ok_or_else(|| r0_journal_error("verify Cell 2 worker projection", "attempt is missing"))?;
    if terminal.attempt() != Some(attempt)
        || terminal.identity() != spawned.identity()
        || terminal.receipt().timestamp() != continuation.committed_at_utc
    {
        return Err(r0_journal_error(
            "verify Cell 2 worker projection",
            "spawn and terminal records do not match their sealed durable evidence",
        ));
    }
    let terminal_event_type = terminal.kind().as_go_str().to_owned();
    let event_types = projector
        .event_log
        .events()
        .iter()
        .map(|event| event.record.event_type.clone())
        .collect::<Vec<_>>();
    let expected = [
        "mission.started",
        "phase.started",
        "worker.spawned",
        terminal_event_type.as_str(),
    ];
    if event_types != expected || checkpoint.status != "in_progress" || phase_status != "running" {
        return Err(r0_journal_error(
            "verify completed Cell 2 continuation",
            format!(
                "events={event_types:?}, mission={}, phase={phase_status}",
                checkpoint.status
            ),
        ));
    }
    let event_bytes = projector
        .event_log
        .events()
        .iter()
        .flat_map(|event| event.raw_line.iter().copied())
        .collect::<Vec<_>>();

    let bounds = ProjectionRecoveryBounds::new(32, 256 * 1024)
        .map_err(|error| r0_journal_error("build Cell 2 recovery bounds", error))?;
    let snapshot = projector
        .store
        .projection_recovery_snapshot(&mission, &bounds)
        .map_err(|error| r0_journal_error("inspect completed Cell 2 journal", error))?;
    let journal_record_count = snapshot.records().len();
    let acknowledged_record_count = snapshot
        .records()
        .iter()
        .filter(|record| record.acknowledgement().is_some())
        .count();
    let projection_receipt_count = snapshot
        .records()
        .iter()
        .map(|record| record.present_receipts().len())
        .sum();
    let incomplete_record_count = snapshot
        .records()
        .iter()
        .filter(|record| {
            !record.missing_projections().is_empty() || record.acknowledgement().is_none()
        })
        .count();
    let phase_start_transition_count = snapshot
        .records()
        .iter()
        .filter(|record| record.transition_kind() == "phase.started")
        .count();
    if journal_record_count != 5
        || acknowledged_record_count != 5
        || projection_receipt_count != 10
        || incomplete_record_count != 0
        || phase_start_transition_count != 1
    {
        return Err(r0_journal_error(
            "verify completed Cell 2 journal",
            format!(
                "records={journal_record_count}, acknowledgements={acknowledged_record_count}, \
                 receipts={projection_receipt_count}, incomplete={incomplete_record_count}, \
                 phase_start_rows={phase_start_transition_count}"
            ),
        ));
    }

    Ok(R0PhaseStartContinuationReport {
        event_types,
        event_bytes,
        checkpoint_bytes,
        mission_status: checkpoint.status,
        phase_status,
        journal_record_count,
        acknowledged_record_count,
        projection_receipt_count,
        incomplete_record_count,
        phase_start_transition_count,
        recovery_checkpoint_disposition,
        worker_id: spawned.identity().worker_id().to_owned(),
        attempt,
        terminal_event_type,
        spawned_at_utc: spawned.receipt().timestamp().to_owned(),
        terminal_at_utc: terminal.receipt().timestamp().to_owned(),
        durable_committed_at_utc: continuation.committed_at_utc,
        projection_replayed: continuation.projection.replayed(),
        durable_attempt: continuation.durable_attempt,
    })
}

fn build_r0_journal_recovery_report(
    projector: &mut HermeticCompatibilityProjector,
    cell: R0JournalCrashCell,
) -> Result<R0JournalRecoveryReport, R0JournalCrashError> {
    let mission = MissionId::new(R0_JOURNAL_MISSION)
        .map_err(|error| r0_journal_error("build mission identity", error))?;
    let checkpoint_bytes = projector.checkpoint_bytes().to_vec();
    let checkpoint = orchestrator_core::decode_checkpoint(&checkpoint_bytes)
        .map_err(|error| r0_journal_error("decode recovered checkpoint", error))?
        .projection;
    let phase_status = checkpoint
        .plan
        .as_ref()
        .and_then(|plan| plan.phases.first())
        .map(|phase| phase.status.clone())
        .ok_or_else(|| r0_journal_error("decode recovered checkpoint", "phase is missing"))?;
    let event_types = projector
        .event_log
        .events()
        .iter()
        .map(|event| event.record.event_type.clone())
        .collect::<Vec<_>>();
    let event_bytes = projector
        .event_log
        .events()
        .iter()
        .flat_map(|event| event.raw_line.iter().copied())
        .collect::<Vec<_>>();
    let expected = match cell {
        R0JournalCrashCell::MissionStartBeforeProjection => ["mission.started"].as_slice(),
        R0JournalCrashCell::PhaseStartBeforeProjection => {
            ["mission.started", "phase.started"].as_slice()
        }
        R0JournalCrashCell::EventAppendBeforeCheckpoint
        | R0JournalCrashCell::CheckpointFsyncBeforeReceipt => {
            ["mission.started", "phase.started", "phase.completed"].as_slice()
        }
    };
    if event_types != expected {
        return Err(r0_journal_error(
            "verify recovered event sequence",
            format!("expected {expected:?}, found {event_types:?}"),
        ));
    }
    let bounds = ProjectionRecoveryBounds::new(32, 256 * 1024)
        .map_err(|error| r0_journal_error("build recovery bounds", error))?;
    let snapshot = projector
        .store
        .projection_recovery_snapshot(&mission, &bounds)
        .map_err(|error| r0_journal_error("inspect recovered journal", error))?;
    let journal_record_count = snapshot.records().len();
    let acknowledged_record_count = snapshot
        .records()
        .iter()
        .filter(|record| record.acknowledgement().is_some())
        .count();
    let projection_receipt_count = snapshot
        .records()
        .iter()
        .map(|record| record.present_receipts().len())
        .sum();
    let incomplete_record_count = snapshot
        .records()
        .iter()
        .filter(|record| {
            !record.missing_projections().is_empty() || record.acknowledgement().is_none()
        })
        .count();
    let target_kind = event_type_str(&cell.transition());
    let target_transition_count = snapshot
        .records()
        .iter()
        .filter(|record| record.transition_kind() == target_kind)
        .count();
    let expected_record_count: usize = match cell {
        R0JournalCrashCell::MissionStartBeforeProjection => 2,
        R0JournalCrashCell::PhaseStartBeforeProjection => 3,
        R0JournalCrashCell::EventAppendBeforeCheckpoint
        | R0JournalCrashCell::CheckpointFsyncBeforeReceipt => 4,
    };
    if journal_record_count != expected_record_count
        || acknowledged_record_count != expected_record_count
        || projection_receipt_count != expected_record_count.saturating_mul(2)
        || incomplete_record_count != 0
        || target_transition_count != 1
    {
        return Err(r0_journal_error(
            "verify recovered journal",
            format!(
                "records={journal_record_count}, acknowledgements={acknowledged_record_count}, \
                 receipts={projection_receipt_count}, incomplete={incomplete_record_count}, \
                 target_rows={target_transition_count}"
            ),
        ));
    }
    let recovery_checkpoint_disposition = projector.last_checkpoint_disposition;
    Ok(R0JournalRecoveryReport {
        event_types,
        event_bytes,
        checkpoint_bytes,
        mission_status: checkpoint.status,
        phase_status,
        journal_record_count,
        acknowledged_record_count,
        projection_receipt_count,
        incomplete_record_count,
        target_transition_count,
        recovery_checkpoint_disposition,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::{
        CheckpointPhase, PhaseStatus, VerificationClass, VerificationMode, VerificationOutcome,
        decide_verification, decode_checkpoint, scan_event_log,
    };
    use orchestrator_exec::{
        AttemptEvidence, Effort, ExecutionRequestDraft, PartialWork, RuntimeFamily,
    };
    use std::{
        fs,
        path::PathBuf,
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    static NEXT: AtomicU64 = AtomicU64::new(1);
    const CRASH_HOME_ENV: &str = "NANIKA_HERMETIC_PROJECTOR_CRASH_HOME";
    const CRASH_POINT_ENV: &str = "NANIKA_HERMETIC_PROJECTOR_CRASH_POINT";

    /// A bare, disposable temp-directory root wrapped as a test-only
    /// `ProductionBoundary` — never a live home, never an existing Go
    /// database. Mirrors `runtime_store.rs`'s own `TestHome` test fixture.
    struct TestHome {
        path: PathBuf,
    }

    impl TestHome {
        fn new(label: &str) -> TestResult<Self> {
            let path = std::env::temp_dir().join(format!(
                "orchestrator-hermetic-projector-{}-{}-{label}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
            }
            Ok(Self {
                path: fs::canonicalize(path)?,
            })
        }

        fn boundary(&self) -> TestResult<Arc<ProductionBoundary>> {
            Ok(Arc::new(ProductionBoundary::from_canonical_root(
                &self.path,
            )?))
        }

        fn workspace_dir(&self, mission: &str) -> PathBuf {
            self.path.join("workspaces").join(mission)
        }

        fn checkpoint_path(&self, mission: &str) -> PathBuf {
            self.workspace_dir(mission).join("checkpoint.json")
        }

        fn mission_md_path(&self, mission: &str) -> PathBuf {
            self.workspace_dir(mission).join("mission.md")
        }

        fn plan_json_path(&self, mission: &str) -> PathBuf {
            self.workspace_dir(mission).join("plan.json")
        }

        fn event_log_path(&self, mission: &str) -> PathBuf {
            self.path.join("events").join(format!("{mission}.jsonl"))
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// Fix 5 (§7.7): recursively asserts the disposable boundary root
    /// contains no leftover temporary/replacement entry after a crash
    /// test's recovery step. Every atomic-replace helper in this crate stages
    /// its write under a name containing one of these substrings before the
    /// final rename, so any survivor proves a publisher left residue behind
    /// on a path recovery is supposed to have fully reconciled.
    fn assert_no_temp_residue(root: &std::path::Path) -> TestResult {
        const FORBIDDEN_SUBSTRINGS: &[&str] =
            &[".tmp", "production-replacement", "fixture-replacement"];

        fn walk(dir: &std::path::Path, offending: &mut Vec<PathBuf>) -> std::io::Result<()> {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if FORBIDDEN_SUBSTRINGS
                    .iter()
                    .any(|forbidden| name.contains(forbidden))
                {
                    offending.push(path.clone());
                }
                if entry.file_type()?.is_dir() {
                    walk(&path, offending)?;
                }
            }
            Ok(())
        }

        let mut offending = Vec::new();
        walk(root, &mut offending)?;
        assert!(
            offending.is_empty(),
            "found residual temp/replacement entries after recovery: {offending:?}"
        );
        Ok(())
    }

    const MISSION: &str = "mission-1";

    fn simple_plan(phase_ids: &[&str]) -> CheckpointPlan {
        CheckpointPlan {
            phases: phase_ids
                .iter()
                .map(|id| CheckpointPhase {
                    id: (*id).to_owned(),
                    status: "pending".to_owned(),
                    ..CheckpointPhase::default()
                })
                .collect(),
            ..CheckpointPlan::default()
        }
    }

    fn mission_seed(markdown: &[u8], plan: CheckpointPlan) -> MissionSeed {
        MissionSeed {
            mission_markdown: markdown.to_vec(),
            plan,
        }
    }

    fn passing_verification() -> VerificationDecision {
        decide_verification(
            VerificationOutcome::Classified(VerificationClass::Pass),
            VerificationMode::Block,
        )
    }

    fn ts(step: u32) -> String {
        format!("2026-07-17T00:00:{step:02}Z")
    }

    fn worker_request(
        home: &TestHome,
        mission: &str,
        phase: &str,
        persona: &str,
    ) -> TestResult<ExecutionRequest> {
        worker_request_for_attempt(home, mission, phase, persona, 1)
    }

    fn worker_request_for_attempt(
        home: &TestHome,
        mission: &str,
        phase: &str,
        persona: &str,
        attempt: u32,
    ) -> TestResult<ExecutionRequest> {
        let phase_id = PhaseId::new(phase)?;
        let worker_id = WorkerId::for_phase(persona, &phase_id)?;
        worker_request_at_path(
            mission,
            phase,
            persona,
            attempt,
            home.workspace_dir(mission)
                .join("workers")
                .join(worker_id.as_str()),
        )
    }

    fn worker_request_at_path(
        mission: &str,
        phase: &str,
        persona: &str,
        attempt: u32,
        worker_dir: PathBuf,
    ) -> TestResult<ExecutionRequest> {
        Ok(ExecutionRequest::new(ExecutionRequestDraft {
            mission: mission.to_owned(),
            phase: phase.to_owned(),
            attempt,
            revision: 1,
            objective: "project one durable worker outcome".to_owned(),
            persona: persona.to_owned(),
            role: "verifier".to_owned(),
            domain: "dev".to_owned(),
            skills: vec!["rust-best-practices".to_owned()],
            dependencies: Vec::new(),
            expected_evidence: Vec::new(),
            constraints: vec!["hermetic".to_owned()],
            prior_context: String::new(),
            runtime: RuntimeFamily::parse("fixture-runtime")?,
            model: "fixture-model".to_owned(),
            effort: Effort::High,
            max_turns: 1,
            worker_dir,
            target_dir: None,
            resume_from: None,
            hook_script: None,
        })?)
    }

    fn append_adversarial_worker_record(
        projector: &mut HermeticCompatibilityProjector,
        transition_id: &str,
        kind: WorkerEventKind,
        binding: &CanonicalWorkerAttempt,
        data: Value,
        committed_at_utc: &str,
    ) -> TestResult {
        let intent = JournalIntent::new(
            transition_id,
            Some(projector.mission_id.clone()),
            kind.as_go_str(),
            encode_worker_payload(binding, &data)?,
            committed_at_utc.to_owned(),
        )?
        .with_required_projection(CompatibilityProjection::EventLog)
        .with_required_projection(CompatibilityProjection::Checkpoint);
        projector.store.append(&intent)?;
        Ok(())
    }

    fn append_canonical_adversarial_worker_record(
        projector: &mut HermeticCompatibilityProjector,
        kind: WorkerEventKind,
        binding: &CanonicalWorkerAttempt,
        data: Value,
        committed_at_utc: &str,
    ) -> TestResult {
        let transition_id = worker_transition_id(&projector.mission_id, binding, kind);
        append_adversarial_worker_record(
            projector,
            &transition_id,
            kind,
            binding,
            data,
            committed_at_utc,
        )
    }

    fn worker_prefix_reopen_error(
        label: &str,
        append: impl FnOnce(
            &mut HermeticCompatibilityProjector,
            &CanonicalWorkerAttempt,
            &ExecutionRequest,
        ) -> TestResult,
    ) -> TestResult<HermeticProjectorError> {
        let home = TestHome::new(label)?;
        let boundary = home.boundary()?;
        let mission_name = format!("worker-prefix-{label}");
        let mission_id = MissionId::new(mission_name.clone())?;
        let plan = simple_plan(&["phase-1"]);
        let mut projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# worker prefix\n", plan.clone()),
            ts(0),
        )?;
        projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        projector.apply_transition(
            Some(PhaseId::new("phase-1")?),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(2),
        )?;
        let request = worker_request(&home, &mission_name, "phase-1", "persona")?;
        let binding = projector.bind_worker_attempt(&request)?;
        append(&mut projector, &binding, &request)?;
        projector.close()?;

        match HermeticCompatibilityProjector::open(
            boundary,
            mission_id,
            mission_seed(b"# worker prefix\n", plan),
            ts(0),
        ) {
            Err(error) => Ok(error),
            Ok(projector) => {
                projector.close()?;
                Err("malformed worker journal prefix was accepted".into())
            }
        }
    }

    fn assert_worker_prefix_rejected(
        label: &str,
        append: impl FnOnce(
            &mut HermeticCompatibilityProjector,
            &CanonicalWorkerAttempt,
            &ExecutionRequest,
        ) -> TestResult,
    ) -> TestResult {
        let error = worker_prefix_reopen_error(label, append)?;
        assert!(matches!(
            error,
            HermeticProjectorError::WorkerStreamConflict
                | HermeticProjectorError::TransitionIdentityConflict
                | HermeticProjectorError::CorruptJournalPayload
                | HermeticProjectorError::WorkerEventCodec(_)
        ));
        Ok(())
    }

    /// Independently re-derives the exact `checkpoint.json` bytes a mission
    /// in `state` must durably have, through the very same
    /// `checkpoint_for_state` + `encode_current_checkpoint` mapping the
    /// projector itself uses — but computed fresh here from only `mission`
    /// and `plan`, not read back from any live projector field. Used by the
    /// byte-exact crash-recovery assertions (§7.7 / Fix 7).
    fn expected_checkpoint_bytes(
        mission: &str,
        plan: &CheckpointPlan,
        state: &MissionState,
    ) -> TestResult<Vec<u8>> {
        let template = CheckpointProjection {
            workspace_id: mission.to_owned(),
            plan: Some(plan.clone()),
            ..CheckpointProjection::default()
        };
        let checkpoint = checkpoint_for_state(&template, state)?;
        Ok(encode_current_checkpoint(&checkpoint)?)
    }

    /// Independently re-derives the exact `MissionState` a durable event log
    /// containing exactly `mission.started` then `phase.started` (on
    /// `phase-1`, the only shape every crash test in this module produces)
    /// must reduce to — using each event's *real*, on-disk id/sequence (read
    /// back, never guessed), not the projector's own retained state. Used by
    /// the byte-exact crash-recovery assertions (§7.7 / Fix 7).
    fn expected_state_after_mission_started_and_phase_started(
        mission_id: &MissionId,
        plan: &CheckpointPlan,
        event_bytes: &[u8],
    ) -> TestResult<MissionState> {
        let scan = scan_event_log(event_bytes);
        assert!(scan.diagnostics.is_empty());
        assert_eq!(scan.events.len(), 2);
        let mut state = build_pristine_state(mission_id, plan)?;
        for event in &scan.events {
            let record = &event.record;
            let phase_id = record.phase_id.as_deref().map(PhaseId::new).transpose()?;
            let transition = parse_transition_kind(&record.event_type, None)?;
            let input = ReducerInput {
                event_id: EventId::new(record.id.clone())?,
                sequence: record.sequence,
                timestamp: record.timestamp.clone(),
                mission_id: mission_id.clone(),
                phase_id,
                worker_id: None,
                data: Value::Null,
                extra: BTreeMap::new(),
                transition,
            };
            state = reduce(&state, &input)?.state;
        }
        Ok(state)
    }

    #[test]
    fn admission_is_idempotent_and_produces_a_pristine_mission() -> TestResult {
        let home = TestHome::new("admission")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);

        let projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        )?;
        assert_eq!(projector.state().status(), MissionStatus::NotStarted);
        assert!(projector.has_checkpoint_writer());
        assert_eq!(projector.mission_id().as_str(), MISSION);
        assert_eq!(projector.mission_markdown(), b"# mission\n");
        assert_eq!(projector.plan().phases.len(), 1);
        projector.close()?;

        // Re-admitting with the exact same content resumes cleanly.
        let projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
        )?;
        assert_eq!(projector.state().status(), MissionStatus::NotStarted);
        projector.close()?;
        Ok(())
    }

    #[test]
    fn admission_rejects_divergent_content_under_the_same_mission() -> TestResult {
        let home = TestHome::new("admission-divergent")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);

        let projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        )?;
        projector.close()?;

        let divergent = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# a different mission\n", plan),
            ts(0),
        );
        assert!(matches!(
            divergent,
            Err(HermeticProjectorError::Store(
                RuntimeStoreError::TransitionConflict
            ))
        ));
        Ok(())
    }

    #[test]
    fn a_second_concurrent_open_is_rejected() -> TestResult {
        let home = TestHome::new("concurrent")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);

        let _first = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        )?;
        let second = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
        );
        assert!(matches!(
            second,
            Err(HermeticProjectorError::Store(
                RuntimeStoreError::WriterLeased
            ))
        ));
        Ok(())
    }

    #[test]
    fn open_releases_the_writer_lease_on_a_genuinely_divergent_checkpoint() -> TestResult {
        // Fix 1 (unchanged by Cell 2E): every error path inside `open()`
        // after the store is opened must release the writer lease (via a
        // best-effort `close()`), or a second `open()` call in the same
        // process would be permanently blocked with `WriterLeased` for the
        // rest of the process's lifetime.
        //
        // Before Cell 2E, this test reused the
        // `after_event_receipt_before_checkpoint_publish` crash point as a
        // convenient, always-on open-time error: recovery failed closed
        // purely because `ProductionProjectionWriter` could not be
        // re-acquired for a non-pristine workspace. Cell 2E's
        // `into_production_projection_writer_recovered` closes that gap
        // (see `crash_after_event_receipt_before_checkpoint_publish_recovers_via_reopen`,
        // which now succeeds at this exact crash point), so it can no
        // longer serve as a reliable error source on its own. This test
        // instead tampers the durable checkpoint afterward to construct a
        // *genuine* divergence — proving both that real corruption still
        // fails closed even through the recovered path (the gap closing did
        // not open a forgery path), and that the failed attempt still
        // releases its lease.
        let home = TestHome::new("open-releases-lease-on-error")?;
        let code = run_crash_child(&home, "after_event_receipt_before_checkpoint_publish")?;
        assert_eq!(code, 75);

        let checkpoint_path = home.checkpoint_path(MISSION);
        let bytes = fs::read(&checkpoint_path)?;
        let tampered =
            String::from_utf8(bytes.clone())?.replacen("\"in_progress\"", "\"completed\"", 1);
        assert_ne!(tampered.as_bytes(), bytes.as_slice());
        fs::write(&checkpoint_path, tampered.as_bytes())?;

        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);

        let first_attempt = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        );
        assert!(matches!(
            first_attempt,
            Err(HermeticProjectorError::ProjectionRecoveryUnavailable(
                CompatibilityProjection::Checkpoint
            ))
        ));

        // The critical assertion: a SECOND `open()` call on the same
        // boundary, in this same process, must reach the identical typed
        // error again — not `RuntimeStoreError::WriterLeased` from a leaked
        // lease the first failed attempt never released.
        let second_attempt = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
        );
        assert!(matches!(
            second_attempt,
            Err(HermeticProjectorError::ProjectionRecoveryUnavailable(
                CompatibilityProjection::Checkpoint
            ))
        ));
        Ok(())
    }

    #[test]
    fn lifecycle_transitions_publish_events_and_checkpoint_in_order() -> TestResult {
        let home = TestHome::new("lifecycle")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mut projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
        )?;

        let outcome = projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        assert!(!outcome.replayed);
        assert_eq!(outcome.mission_status, MissionStatus::InProgress);

        let phase = PhaseId::new("phase-1")?;
        projector.apply_transition(
            Some(phase.clone()),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(2),
        )?;
        projector.apply_transition(
            Some(phase.clone()),
            ReducerTransition::PhaseCompleted,
            Value::Null,
            Some(passing_verification()),
            ts(3),
        )?;
        let outcome = projector.apply_transition(
            None,
            ReducerTransition::MissionCompleted,
            Value::Null,
            None,
            ts(4),
        )?;
        assert_eq!(outcome.mission_status, MissionStatus::Completed);
        let expected_checkpoint_bytes = projector.checkpoint_bytes().to_vec();
        projector.close()?;

        // Frozen-Go-compatible readback against the disposable root: decode
        // the exact published checkpoint.json and events.jsonl bytes with
        // the same decoders a Go reader would trust, never a live database.
        let on_disk_checkpoint = fs::read(home.checkpoint_path(MISSION))?;
        assert_eq!(on_disk_checkpoint, expected_checkpoint_bytes);
        let decoded = decode_checkpoint(&on_disk_checkpoint)?;
        assert_eq!(decoded.projection.status, "completed");
        assert_eq!(
            decoded
                .projection
                .plan
                .ok_or("missing plan")?
                .phases
                .first()
                .ok_or("missing phase")?
                .status,
            "completed"
        );

        let event_bytes = fs::read(home.event_log_path(MISSION))?;
        let scan = scan_event_log(&event_bytes);
        assert!(scan.diagnostics.is_empty());
        assert_eq!(scan.events.len(), 4);
        let expected_types = [
            "mission.started",
            "phase.started",
            "phase.completed",
            "mission.completed",
        ];
        for (index, (event, expected_type)) in scan.events.iter().zip(expected_types).enumerate() {
            assert_eq!(event.record.event_type, expected_type);
            assert_eq!(event.record.sequence, i64::try_from(index + 1)?);
        }
        Ok(())
    }

    #[test]
    fn active_worker_blocks_terminal_lifecycle_without_durable_mutation() -> TestResult {
        const WORKER_MISSION: &str = "worker-active-lifecycle";
        let home = TestHome::new("worker-active-lifecycle")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(WORKER_MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mut projector = HermeticCompatibilityProjector::open(
            boundary,
            mission_id.clone(),
            mission_seed(b"# active worker lifecycle\n", plan),
            ts(0),
        )?;
        projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        let phase = PhaseId::new("phase-1")?;
        projector.apply_transition(
            Some(phase.clone()),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(2),
        )?;
        let request = worker_request(&home, WORKER_MISSION, phase.as_str(), "persona")?;
        let binding = projector.bind_worker_attempt(&request)?;
        projector.project_canonical_worker_event(
            &binding,
            WorkerEventKind::Spawned,
            worker_spawned_data(&request, binding.attempt),
            &ts(3),
        )?;

        let event_bytes = fs::read(home.event_log_path(WORKER_MISSION))?;
        let checkpoint_bytes = fs::read(home.checkpoint_path(WORKER_MISSION))?;
        let record_count = projector
            .store
            .projection_recovery_snapshot(
                &mission_id,
                &ProjectionRecoveryBounds::new(RECOVERY_MAX_RECORDS, RECOVERY_MAX_BYTES)?,
            )?
            .records()
            .len();

        for (phase_id, transition, verification) in [
            (
                Some(phase.clone()),
                ReducerTransition::PhaseCompleted,
                Some(passing_verification()),
            ),
            (
                Some(phase.clone()),
                ReducerTransition::PhaseFailed {
                    error: "forced failure".to_owned(),
                },
                None,
            ),
            (
                Some(phase.clone()),
                ReducerTransition::PhaseSkipped {
                    reason: "forced skip".to_owned(),
                },
                None,
            ),
            (None, ReducerTransition::MissionCompleted, None),
            (None, ReducerTransition::MissionFailed, None),
            (
                None,
                ReducerTransition::MissionCancelled {
                    reason: "forced cancel".to_owned(),
                },
                None,
            ),
        ] {
            let result =
                projector.apply_transition(phase_id, transition, Value::Null, verification, ts(4));
            assert!(matches!(
                result,
                Err(HermeticProjectorError::ActiveWorkerAttempt)
            ));
        }

        assert_eq!(fs::read(home.event_log_path(WORKER_MISSION))?, event_bytes);
        assert_eq!(
            fs::read(home.checkpoint_path(WORKER_MISSION))?,
            checkpoint_bytes
        );
        assert_eq!(
            projector
                .store
                .projection_recovery_snapshot(
                    &mission_id,
                    &ProjectionRecoveryBounds::new(RECOVERY_MAX_RECORDS, RECOVERY_MAX_BYTES,)?,
                )?
                .records()
                .len(),
            record_count
        );
        assert_eq!(
            projector
                .state()
                .phase(&phase)
                .ok_or("running phase disappeared")?
                .status,
            PhaseStatus::Running
        );
        projector.close()?;
        Ok(())
    }

    #[test]
    fn recovery_rejects_phase_terminal_after_unterminated_worker() -> TestResult {
        const WORKER_MISSION: &str = "worker-active-recovery";
        let home = TestHome::new("worker-active-recovery")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(WORKER_MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mut projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# active worker recovery\n", plan.clone()),
            ts(0),
        )?;
        projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        let phase = PhaseId::new("phase-1")?;
        projector.apply_transition(
            Some(phase.clone()),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(2),
        )?;
        let request = worker_request(&home, WORKER_MISSION, phase.as_str(), "persona")?;
        let binding = projector.bind_worker_attempt(&request)?;
        projector.project_canonical_worker_event(
            &binding,
            WorkerEventKind::Spawned,
            worker_spawned_data(&request, binding.attempt),
            &ts(3),
        )?;
        let event_bytes = fs::read(home.event_log_path(WORKER_MISSION))?;
        let checkpoint_bytes = fs::read(home.checkpoint_path(WORKER_MISSION))?;

        let event_type = event_type_str(&ReducerTransition::PhaseCompleted);
        let intent = JournalIntent::new(
            lifecycle_transition_id(&mission_id, Some(&phase), event_type),
            Some(mission_id.clone()),
            event_type,
            encode_payload(Some(&phase), &Value::Null, None)?,
            ts(4),
        )?
        .with_required_projection(CompatibilityProjection::EventLog)
        .with_required_projection(CompatibilityProjection::Checkpoint);
        projector.store.append(&intent)?;
        projector.close()?;

        let reopened = HermeticCompatibilityProjector::open(
            boundary,
            mission_id,
            mission_seed(b"# active worker recovery\n", plan),
            ts(0),
        );
        assert!(matches!(
            reopened,
            Err(HermeticProjectorError::ActiveWorkerAttempt)
        ));
        assert_eq!(fs::read(home.event_log_path(WORKER_MISSION))?, event_bytes);
        assert_eq!(
            fs::read(home.checkpoint_path(WORKER_MISSION))?,
            checkpoint_bytes
        );
        Ok(())
    }

    #[test]
    fn canonical_completed_worker_projection_is_bound_and_byte_stable_across_replay() -> TestResult
    {
        const WORKER_MISSION: &str = "worker-completed";
        let home = TestHome::new("worker-completed")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(WORKER_MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mut projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# worker completed\n", plan.clone()),
            ts(0),
        )?;
        projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        projector.apply_transition(
            Some(PhaseId::new("phase-1")?),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(2),
        )?;
        let checkpoint_before = projector.checkpoint_bytes().to_vec();
        let request = worker_request(&home, WORKER_MISSION, "phase-1", "persona")?;
        let outcome =
            AttemptOutcome::completed("done", AttemptEvidence::new(), Duration::from_secs(2))?;

        let first = projector.project_durable_worker_attempt(
            DurableWorkerProjectionInput::from_test_outcome(&request, &outcome, &ts(3)),
        )?;
        assert!(!first.replayed());
        assert_eq!(first.spawned().kind(), WorkerEventKind::Spawned);
        assert_eq!(first.terminal().kind(), WorkerEventKind::Completed);
        assert_eq!(first.spawned().attempt(), Some(1));
        assert_eq!(first.terminal().attempt(), Some(1));
        assert_eq!(first.spawned().identity().worker_id(), "persona-phase-1");
        assert_eq!(first.terminal().identity(), first.spawned().identity());
        assert_eq!(projector.checkpoint_bytes(), checkpoint_before);

        let first_bytes = fs::read(home.event_log_path(WORKER_MISSION))?;
        let scan = scan_event_log(&first_bytes);
        assert!(scan.diagnostics.is_empty());
        assert_eq!(
            scan.events
                .iter()
                .map(|event| event.record.event_type.as_str())
                .collect::<Vec<_>>(),
            [
                "mission.started",
                "phase.started",
                "worker.spawned",
                "worker.completed"
            ]
        );
        assert_eq!(
            scan.events
                .iter()
                .map(|event| event.record.sequence)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );

        let same_session = projector.project_durable_worker_attempt(
            DurableWorkerProjectionInput::from_test_outcome(&request, &outcome, &ts(3)),
        )?;
        assert!(same_session.replayed());
        assert_eq!(fs::read(home.event_log_path(WORKER_MISSION))?, first_bytes);
        projector.close()?;

        let mut reopened = HermeticCompatibilityProjector::open(
            boundary,
            mission_id,
            mission_seed(b"# worker completed\n", plan),
            ts(0),
        )?;
        let reopened_replay = reopened.project_durable_worker_attempt(
            DurableWorkerProjectionInput::from_test_outcome(&request, &outcome, &ts(3)),
        )?;
        assert!(reopened_replay.replayed());
        assert_eq!(fs::read(home.event_log_path(WORKER_MISSION))?, first_bytes);
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn canonical_failed_worker_projection_is_typed_and_byte_stable() -> TestResult {
        const WORKER_MISSION: &str = "worker-failed";
        let home = TestHome::new("worker-failed")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(WORKER_MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mut projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# worker failed\n", plan.clone()),
            ts(0),
        )?;
        projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        projector.apply_transition(
            Some(PhaseId::new("phase-1")?),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(2),
        )?;
        let request = worker_request(&home, WORKER_MISSION, "phase-1", "persona")?;
        let outcome = AttemptOutcome::incomplete(
            MechanicalTermination::SupervisorFailure,
            None,
            PartialWork::empty(),
            Duration::from_millis(7),
        );
        let first = projector.project_durable_worker_attempt(
            DurableWorkerProjectionInput::from_test_outcome(&request, &outcome, &ts(3)),
        )?;
        assert_eq!(first.terminal().kind(), WorkerEventKind::Failed);
        let first_bytes = fs::read(home.event_log_path(WORKER_MISSION))?;
        projector.close()?;

        let mut reopened = HermeticCompatibilityProjector::open(
            boundary,
            mission_id,
            mission_seed(b"# worker failed\n", plan),
            ts(0),
        )?;
        let replay = reopened.project_durable_worker_attempt(
            DurableWorkerProjectionInput::from_test_outcome(&request, &outcome, &ts(3)),
        )?;
        assert!(replay.replayed());
        assert_eq!(fs::read(home.event_log_path(WORKER_MISSION))?, first_bytes);
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn spawn_only_reopen_projects_terminal_with_a_new_durable_timestamp() -> TestResult {
        const WORKER_MISSION: &str = "worker-spawn-only";
        let home = TestHome::new("worker-spawn-only")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(WORKER_MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mut projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# worker spawn only\n", plan.clone()),
            ts(0),
        )?;
        projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        projector.apply_transition(
            Some(PhaseId::new("phase-1")?),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(2),
        )?;
        let request = worker_request(&home, WORKER_MISSION, "phase-1", "persona")?;
        let binding = projector.bind_worker_attempt(&request)?;
        let (spawned, replayed) = projector.project_canonical_worker_event(
            &binding,
            WorkerEventKind::Spawned,
            worker_spawned_data(&request, binding.attempt),
            &ts(3),
        )?;
        assert!(!replayed);
        assert_eq!(spawned.receipt().timestamp(), ts(3));
        let spawn_only_bytes = fs::read(home.event_log_path(WORKER_MISSION))?;
        let later_request =
            worker_request_for_attempt(&home, WORKER_MISSION, "phase-1", "persona", 2)?;
        let later_outcome =
            AttemptOutcome::completed("later", AttemptEvidence::new(), Duration::ZERO)?;
        let active_conflict = projector.project_durable_worker_attempt(
            DurableWorkerProjectionInput::from_test_outcome(&later_request, &later_outcome, &ts(4)),
        );
        assert!(matches!(
            active_conflict,
            Err(HermeticProjectorError::WorkerStreamConflict)
        ));
        assert_eq!(
            fs::read(home.event_log_path(WORKER_MISSION))?,
            spawn_only_bytes
        );
        projector.close()?;

        let outcome =
            AttemptOutcome::completed("done", AttemptEvidence::new(), Duration::from_secs(1))?;
        let mut reopened = HermeticCompatibilityProjector::open(
            boundary,
            mission_id,
            mission_seed(b"# worker spawn only\n", plan),
            ts(0),
        )?;
        let repaired = reopened.project_durable_worker_attempt(
            DurableWorkerProjectionInput::from_test_outcome(&request, &outcome, &ts(9)),
        )?;
        assert!(!repaired.replayed());
        assert_eq!(repaired.spawned().receipt().timestamp(), ts(3));
        assert_eq!(repaired.terminal().receipt().timestamp(), ts(9));
        let repaired_bytes = fs::read(home.event_log_path(WORKER_MISSION))?;
        assert!(repaired_bytes.starts_with(&spawn_only_bytes));
        let scan = scan_event_log(&repaired_bytes);
        assert!(scan.diagnostics.is_empty());
        assert_eq!(
            scan.events
                .iter()
                .map(|event| event.record.event_type.as_str())
                .collect::<Vec<_>>(),
            [
                "mission.started",
                "phase.started",
                "worker.spawned",
                "worker.completed"
            ]
        );
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn worker_partial_projection_receipts_repair_before_exact_replay() -> TestResult {
        const WORKER_MISSION: &str = "worker-partial-receipt";
        let home = TestHome::new("worker-partial-receipt")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(WORKER_MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mut projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# worker partial receipt\n", plan.clone()),
            ts(0),
        )?;
        projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        projector.apply_transition(
            Some(PhaseId::new("phase-1")?),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(2),
        )?;
        let request = worker_request(&home, WORKER_MISSION, "phase-1", "persona")?;
        let binding = projector.bind_worker_attempt(&request)?;
        projector.project_canonical_worker_event(
            &binding,
            WorkerEventKind::Spawned,
            worker_spawned_data(&request, binding.attempt),
            &ts(3),
        )?;
        let outcome =
            AttemptOutcome::completed("done", AttemptEvidence::new(), Duration::from_millis(4))?;
        let (terminal_kind, terminal_data) = worker_terminal_data(&outcome, binding.attempt);
        projector.r0_crash_point = Some(R0ProjectionCrashPoint::EventPublish);
        let cut = projector.project_canonical_worker_event(
            &binding,
            terminal_kind,
            terminal_data,
            &ts(4),
        );
        assert!(matches!(
            cut,
            Err(HermeticProjectorError::R0InjectedCrashCut("event-publish"))
        ));
        let cut_bytes = fs::read(home.event_log_path(WORKER_MISSION))?;
        projector.close()?;

        let mut reopened = HermeticCompatibilityProjector::open(
            boundary,
            mission_id.clone(),
            mission_seed(b"# worker partial receipt\n", plan),
            ts(0),
        )?;
        let snapshot = reopened.store.projection_recovery_snapshot(
            &mission_id,
            &ProjectionRecoveryBounds::new(RECOVERY_MAX_RECORDS, RECOVERY_MAX_BYTES)?,
        )?;
        assert!(
            snapshot
                .records()
                .iter()
                .all(|record| record.missing_projections().is_empty())
        );
        let replay = reopened.project_durable_worker_attempt(
            DurableWorkerProjectionInput::from_test_outcome(&request, &outcome, &ts(4)),
        )?;
        assert!(replay.replayed());
        assert_eq!(fs::read(home.event_log_path(WORKER_MISSION))?, cut_bytes);
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn terminal_slot_rejects_opposite_outcome_after_journal_commit_cut() -> TestResult {
        const WORKER_MISSION: &str = "worker-terminal-slot";
        let home = TestHome::new("worker-terminal-slot")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(WORKER_MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mut projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# worker terminal slot\n", plan.clone()),
            ts(0),
        )?;
        projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        projector.apply_transition(
            Some(PhaseId::new("phase-1")?),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(2),
        )?;
        let request = worker_request(&home, WORKER_MISSION, "phase-1", "persona")?;
        let binding = projector.bind_worker_attempt(&request)?;
        projector.project_canonical_worker_event(
            &binding,
            WorkerEventKind::Spawned,
            worker_spawned_data(&request, binding.attempt),
            &ts(3),
        )?;
        let completed = AttemptOutcome::completed("done", AttemptEvidence::new(), Duration::ZERO)?;
        let (completed_kind, completed_data) = worker_terminal_data(&completed, binding.attempt);
        projector.r0_crash_point = Some(R0ProjectionCrashPoint::JournalCommit);
        let cut = projector.project_canonical_worker_event(
            &binding,
            completed_kind,
            completed_data,
            &ts(4),
        );
        assert!(matches!(
            cut,
            Err(HermeticProjectorError::R0InjectedCrashCut("journal-commit"))
        ));

        projector.r0_crash_point = None;
        let failed = AttemptOutcome::incomplete(
            MechanicalTermination::SupervisorFailure,
            None,
            PartialWork::empty(),
            Duration::ZERO,
        );
        let (failed_kind, failed_data) = worker_terminal_data(&failed, binding.attempt);
        let conflict =
            projector.project_canonical_worker_event(&binding, failed_kind, failed_data, &ts(4));
        assert!(matches!(
            conflict,
            Err(HermeticProjectorError::Store(
                RuntimeStoreError::TransitionConflict
            ))
        ));
        projector.close()?;

        let reopened = HermeticCompatibilityProjector::open(
            boundary,
            mission_id,
            mission_seed(b"# worker terminal slot\n", plan),
            ts(0),
        )?;
        let bytes = fs::read(home.event_log_path(WORKER_MISSION))?;
        let scan = scan_event_log(&bytes);
        assert!(scan.diagnostics.is_empty());
        assert_eq!(
            scan.events
                .iter()
                .filter(|event| event.record.event_type.starts_with("worker."))
                .map(|event| event.record.event_type.as_str())
                .collect::<Vec<_>>(),
            ["worker.spawned", "worker.completed"]
        );
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn canonical_worker_projection_rejects_conflicting_exact_retry() -> TestResult {
        const WORKER_MISSION: &str = "worker-conflict";
        let home = TestHome::new("worker-conflict")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(WORKER_MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mut projector = HermeticCompatibilityProjector::open(
            boundary,
            mission_id,
            mission_seed(b"# worker conflict\n", plan),
            ts(0),
        )?;
        projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        projector.apply_transition(
            Some(PhaseId::new("phase-1")?),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(2),
        )?;
        let request = worker_request(&home, WORKER_MISSION, "phase-1", "persona")?;
        let completed = AttemptOutcome::completed("done", AttemptEvidence::new(), Duration::ZERO)?;
        projector.project_durable_worker_attempt(
            DurableWorkerProjectionInput::from_test_outcome(&request, &completed, &ts(3)),
        )?;
        let bytes = fs::read(home.event_log_path(WORKER_MISSION))?;

        let changed_timestamp = projector.project_durable_worker_attempt(
            DurableWorkerProjectionInput::from_test_outcome(&request, &completed, &ts(4)),
        );
        assert!(matches!(
            changed_timestamp,
            Err(HermeticProjectorError::Store(
                RuntimeStoreError::TransitionConflict
            ))
        ));
        let failed = AttemptOutcome::incomplete(
            MechanicalTermination::SupervisorFailure,
            None,
            PartialWork::empty(),
            Duration::ZERO,
        );
        let changed_terminal = projector.project_durable_worker_attempt(
            DurableWorkerProjectionInput::from_test_outcome(&request, &failed, &ts(3)),
        );
        assert!(matches!(
            changed_terminal,
            Err(HermeticProjectorError::WorkerStreamConflict)
        ));
        let wrong_path_request = worker_request_at_path(
            WORKER_MISSION,
            "phase-1",
            "persona",
            1,
            std::env::temp_dir().join("persona-phase-1"),
        )?;
        let wrong_path = projector.project_durable_worker_attempt(
            DurableWorkerProjectionInput::from_test_outcome(
                &wrong_path_request,
                &completed,
                &ts(3),
            ),
        );
        assert!(matches!(
            wrong_path,
            Err(HermeticProjectorError::WorkerRequestBindingMismatch)
        ));
        assert_eq!(fs::read(home.event_log_path(WORKER_MISSION))?, bytes);
        projector.close()?;
        Ok(())
    }

    #[test]
    fn worker_projection_requires_the_journal_derived_sole_running_phase() -> TestResult {
        const WORKER_MISSION: &str = "worker-sole-running-phase";
        let home = TestHome::new("worker-sole-running-phase")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(WORKER_MISSION)?;
        let plan = simple_plan(&["phase-1", "phase-2"]);
        let mut projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# worker sole running phase\n", plan.clone()),
            ts(0),
        )?;
        projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        projector.apply_transition(
            Some(PhaseId::new("phase-1")?),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(2),
        )?;
        projector.apply_transition(
            Some(PhaseId::new("phase-2")?),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(3),
        )?;
        let request = worker_request(&home, WORKER_MISSION, "phase-1", "persona")?;
        let outcome = AttemptOutcome::completed("done", AttemptEvidence::new(), Duration::ZERO)?;
        let live = projector.project_durable_worker_attempt(
            DurableWorkerProjectionInput::from_test_outcome(&request, &outcome, &ts(4)),
        );
        assert!(matches!(
            live,
            Err(HermeticProjectorError::WorkerRunningPhaseRequired)
        ));

        let phase_id = PhaseId::new("phase-1")?;
        let worker_id = WorkerId::for_phase("persona", &phase_id)?;
        let binding = CanonicalWorkerAttempt {
            phase_id,
            worker_id: worker_id.as_str().to_owned(),
            attempt: 1,
        };
        append_canonical_adversarial_worker_record(
            &mut projector,
            WorkerEventKind::Spawned,
            &binding,
            worker_spawned_data(&request, binding.attempt),
            &ts(4),
        )?;
        projector.close()?;

        let reopened = HermeticCompatibilityProjector::open(
            boundary,
            mission_id,
            mission_seed(b"# worker sole running phase\n", plan),
            ts(0),
        );
        assert!(matches!(
            reopened,
            Err(HermeticProjectorError::WorkerStreamConflict)
        ));
        Ok(())
    }

    #[test]
    fn worker_recovery_rejects_foreign_transition_identity() -> TestResult {
        let error = worker_prefix_reopen_error(
            "foreign-transition-identity",
            |projector, binding, request| {
                append_adversarial_worker_record(
                    projector,
                    "foreign-worker-transition",
                    WorkerEventKind::Spawned,
                    binding,
                    worker_spawned_data(request, binding.attempt),
                    &ts(3),
                )
            },
        )?;
        assert!(matches!(
            error,
            HermeticProjectorError::TransitionIdentityConflict
        ));
        Ok(())
    }

    #[test]
    fn worker_recovery_rejects_every_malformed_or_conflicting_prefix() -> TestResult {
        assert_worker_prefix_rejected("missing-attempt", |projector, binding, request| {
            let mut spawned = worker_spawned_data(request, binding.attempt);
            spawned
                .as_object_mut()
                .ok_or("spawned data is not an object")?
                .remove("attempt");
            append_canonical_adversarial_worker_record(
                projector,
                WorkerEventKind::Spawned,
                binding,
                spawned,
                &ts(3),
            )
        })?;
        assert_worker_prefix_rejected("terminal-first", |projector, binding, _request| {
            append_canonical_adversarial_worker_record(
                projector,
                WorkerEventKind::Completed,
                binding,
                serde_json::json!({"output_len": 0, "duration": "0s", "attempt": 1}),
                &ts(3),
            )
        })?;
        assert_worker_prefix_rejected("duplicate-spawn", |projector, binding, request| {
            let spawned = worker_spawned_data(request, binding.attempt);
            append_canonical_adversarial_worker_record(
                projector,
                WorkerEventKind::Spawned,
                binding,
                spawned.clone(),
                &ts(3),
            )?;
            append_adversarial_worker_record(
                projector,
                "adversarial:duplicate-spawn:2",
                WorkerEventKind::Spawned,
                binding,
                spawned,
                &ts(3),
            )
        })?;
        assert_worker_prefix_rejected("terminal-conflict", |projector, binding, request| {
            append_canonical_adversarial_worker_record(
                projector,
                WorkerEventKind::Spawned,
                binding,
                worker_spawned_data(request, binding.attempt),
                &ts(3),
            )?;
            append_canonical_adversarial_worker_record(
                projector,
                WorkerEventKind::Completed,
                binding,
                serde_json::json!({"output_len": 0, "duration": "0s", "attempt": 1}),
                &ts(3),
            )?;
            append_adversarial_worker_record(
                projector,
                "adversarial:terminal-conflict:failed",
                WorkerEventKind::Failed,
                binding,
                serde_json::json!({"error": "conflict", "duration": "0s", "attempt": 1}),
                &ts(3),
            )
        })?;
        assert_worker_prefix_rejected("wrong-worker", |projector, binding, request| {
            let mut wrong = binding.clone();
            wrong.worker_id = "other-phase-1".to_owned();
            append_canonical_adversarial_worker_record(
                projector,
                WorkerEventKind::Spawned,
                &wrong,
                worker_spawned_data(request, binding.attempt),
                &ts(3),
            )
        })?;
        assert_worker_prefix_rejected("wrong-worker-root", |projector, binding, request| {
            let mut spawned = worker_spawned_data(request, binding.attempt);
            spawned
                .as_object_mut()
                .ok_or("spawned data is not an object")?
                .insert(
                    "dir".to_owned(),
                    Value::String(format!("/tmp/{}", binding.worker_id)),
                );
            append_canonical_adversarial_worker_record(
                projector,
                WorkerEventKind::Spawned,
                binding,
                spawned,
                &ts(3),
            )
        })?;
        assert_worker_prefix_rejected("active-attempt-sibling", |projector, binding, request| {
            append_canonical_adversarial_worker_record(
                projector,
                WorkerEventKind::Spawned,
                binding,
                worker_spawned_data(request, binding.attempt),
                &ts(3),
            )?;
            let mut later = binding.clone();
            later.attempt = 2;
            append_canonical_adversarial_worker_record(
                projector,
                WorkerEventKind::Spawned,
                &later,
                worker_spawned_data(request, later.attempt),
                &ts(4),
            )
        })?;
        assert_worker_prefix_rejected("attempt-regression", |projector, binding, request| {
            let mut later = binding.clone();
            later.attempt = 2;
            append_canonical_adversarial_worker_record(
                projector,
                WorkerEventKind::Spawned,
                &later,
                worker_spawned_data(request, later.attempt),
                &ts(3),
            )?;
            append_canonical_adversarial_worker_record(
                projector,
                WorkerEventKind::Completed,
                &later,
                serde_json::json!({"output_len": 0, "duration": "0s", "attempt": 2}),
                &ts(4),
            )?;
            append_canonical_adversarial_worker_record(
                projector,
                WorkerEventKind::Spawned,
                binding,
                worker_spawned_data(request, binding.attempt),
                &ts(5),
            )
        })?;
        assert_worker_prefix_rejected("unknown-data", |projector, binding, request| {
            let mut spawned = worker_spawned_data(request, binding.attempt);
            spawned
                .as_object_mut()
                .ok_or("spawned data is not an object")?
                .insert("future_field".to_owned(), Value::Bool(true));
            append_canonical_adversarial_worker_record(
                projector,
                WorkerEventKind::Spawned,
                binding,
                spawned,
                &ts(3),
            )
        })?;
        assert_worker_prefix_rejected("worker-reason", |projector, binding, request| {
            let mut payload =
                encode_worker_payload(binding, &worker_spawned_data(request, binding.attempt))?;
            payload
                .as_object_mut()
                .ok_or("worker payload is not an object")?
                .insert(
                    "reason".to_owned(),
                    Value::String("not canonical".to_owned()),
                );
            let transition_id =
                worker_transition_id(&projector.mission_id, binding, WorkerEventKind::Spawned);
            let intent = JournalIntent::new(
                transition_id,
                Some(projector.mission_id.clone()),
                WorkerEventKind::Spawned.as_go_str(),
                payload,
                ts(3),
            )?
            .with_required_projection(CompatibilityProjection::EventLog)
            .with_required_projection(CompatibilityProjection::Checkpoint);
            projector.store.append(&intent)?;
            Ok(())
        })?;
        Ok(())
    }

    #[test]
    fn phase_completion_without_a_passing_gate_is_rejected() -> TestResult {
        let home = TestHome::new("verification-gate")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mut projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
        )?;
        projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        let phase = PhaseId::new("phase-1")?;
        projector.apply_transition(
            Some(phase.clone()),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(2),
        )?;
        let result = projector.apply_transition(
            Some(phase),
            ReducerTransition::PhaseCompleted,
            Value::Null,
            None,
            ts(3),
        );
        assert!(matches!(
            result,
            Err(HermeticProjectorError::VerificationNotPassed)
        ));
        Ok(())
    }

    #[test]
    fn illegal_transition_is_rejected_without_journaling_and_does_not_burn_an_identity()
    -> TestResult {
        // Fix 2 (§7.6): `PhaseCompleted` on a phase that was never started
        // must be rejected by the reducer's domain check BEFORE it is ever
        // journaled — otherwise the journal would carry a permanent row
        // requiring `EventLog`/`Checkpoint` projections this composition can
        // never satisfy (it can never publish an event/checkpoint for a
        // transition `reduce` rejects), and every future `open()` replaying
        // that row would fail identically, forever.
        let home = TestHome::new("illegal-transition-no-journal")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mut projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        )?;
        projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;

        // Illegal: `PhaseCompleted` on `phase-1`, which was never started
        // (`PhaseStarted` was never applied).
        let phase = PhaseId::new("phase-1")?;
        let result = projector.apply_transition(
            Some(phase.clone()),
            ReducerTransition::PhaseCompleted,
            Value::Null,
            Some(passing_verification()),
            ts(2),
        );
        assert!(matches!(
            result,
            Err(HermeticProjectorError::Transition(
                TransitionError::PhaseNotRunning { .. }
            ))
        ));

        // No journal row: the event log holds only the one prior,
        // legitimate `mission.started` event — the illegal attempt appended
        // nothing.
        let scan = scan_event_log(&fs::read(home.event_log_path(MISSION))?);
        assert_eq!(scan.events.len(), 1);

        // A legitimate transition on the same mission (same live instance,
        // right after the rejected attempt) succeeds with the correct next
        // public sequence (2, not 3): the illegal attempt never burned an
        // identity.
        projector.apply_transition(
            Some(phase),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(3),
        )?;
        let scan = scan_event_log(&fs::read(home.event_log_path(MISSION))?);
        assert_eq!(scan.events.len(), 2);
        assert_eq!(scan.events[1].record.event_type, "phase.started");
        assert_eq!(scan.events[1].record.sequence, 2);
        projector.close()?;

        // Prove via a fresh reopen (process-equivalent recovery) that the
        // mission remains fully usable — replay never encounters the
        // illegal attempt (nothing about it was ever durable) and correctly
        // reconstructs both legitimate transitions.
        let reopened = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
        )?;
        assert_eq!(reopened.state().status(), MissionStatus::InProgress);
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn unsupported_transition_kinds_are_rejected() -> TestResult {
        let home = TestHome::new("unsupported")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mut projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
        )?;
        let result = projector.apply_transition(
            None,
            ReducerTransition::PhaseRetrying,
            Value::Null,
            None,
            ts(1),
        );
        assert!(matches!(
            result,
            Err(HermeticProjectorError::UnsupportedTransition)
        ));
        let result = projector.apply_transition(
            None,
            ReducerTransition::Unknown {
                event_type: "worker.spawned".to_owned(),
                data: Value::Null,
            },
            Value::Null,
            None,
            ts(2),
        );
        assert!(matches!(
            result,
            Err(HermeticProjectorError::UnsupportedTransition)
        ));
        Ok(())
    }

    #[test]
    fn exact_transition_retry_is_idempotent() -> TestResult {
        let home = TestHome::new("retry")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mut projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
        )?;
        let first = projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        assert!(!first.replayed);
        let second = projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        assert!(second.replayed);
        assert_eq!(first.mission_status, second.mission_status);

        let scan = scan_event_log(&fs::read(home.event_log_path(MISSION))?);
        assert_eq!(scan.events.len(), 1);
        Ok(())
    }

    #[test]
    fn phase_failed_reason_round_trips_through_reopen_recovery() -> TestResult {
        let home = TestHome::new("phase-failed-recovery")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mut projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        )?;
        projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        let phase = PhaseId::new("phase-1")?;
        projector.apply_transition(
            Some(phase.clone()),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(2),
        )?;
        projector.apply_transition(
            Some(phase),
            ReducerTransition::PhaseFailed {
                error: "boom".to_owned(),
            },
            Value::Null,
            None,
            ts(3),
        )?;
        projector.close()?;

        // Reopen in a fresh projector value (same content) and confirm the
        // failure reason round-tripped through the journal alone, not
        // through any retained in-memory state.
        let reopened = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
        )?;
        let phase_id = PhaseId::new("phase-1")?;
        let phase_state = reopened.state().phase(&phase_id).ok_or("missing phase")?;
        assert_eq!(phase_state.status, PhaseStatus::Failed);
        assert_eq!(phase_state.error.as_deref(), Some("boom"));
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn corrupted_on_disk_checkpoint_fails_closed_on_reopen() -> TestResult {
        let home = TestHome::new("corrupt-checkpoint")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        )?;
        projector.close()?;

        let checkpoint_path = home.checkpoint_path(MISSION);
        let mut bytes = fs::read(&checkpoint_path)?;
        // Divergent, but still structurally valid JSON: flip the recorded
        // status away from what admission actually published, simulating a
        // same-shape tampered checkpoint rather than a parse failure.
        let corrupted = String::from_utf8(bytes.clone())?.replacen("\"pending\"", "\"running\"", 1);
        assert_ne!(corrupted.as_bytes(), bytes.as_slice());
        bytes = corrupted.into_bytes();
        fs::write(&checkpoint_path, &bytes)?;

        let reopened = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
        );
        assert!(reopened.is_err());
        Ok(())
    }

    #[test]
    fn truncated_event_log_tail_fails_closed_on_reopen() -> TestResult {
        let home = TestHome::new("truncated-events")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mut projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        )?;
        projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        projector.close()?;

        let event_path = home.event_log_path(MISSION);
        let bytes = fs::read(&event_path)?;
        // A missing trailing newline alone is a tolerated torn tail (Go
        // compatibility for a legacy final unterminated record), so truncate
        // deep enough to cut into the JSON object itself — genuine
        // corruption, not a tolerated edge case.
        assert!(bytes.len() > 10, "event log fixture is unexpectedly short");
        fs::write(&event_path, &bytes[..bytes.len() - 10])?;

        let reopened = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
        );
        assert!(reopened.is_err());
        Ok(())
    }

    #[test]
    fn recovered_journal_payload_of_the_wrong_shape_fails_closed() -> TestResult {
        // A payload the projector never produces itself (e.g. an
        // arbitrary-looking journal/receipt mismatch) must fail closed
        // rather than be guessed at during recovery.
        assert!(matches!(
            decode_payload("{\"unexpected_field\": 1}"),
            Err(HermeticProjectorError::CorruptJournalPayload)
        ));
        assert!(matches!(
            decode_payload("not json"),
            Err(HermeticProjectorError::CorruptJournalPayload)
        ));
        assert!(matches!(
            parse_transition_kind("mission.started", Some("unexpected".to_owned())),
            Err(HermeticProjectorError::CorruptJournalPayload)
        ));
        assert!(matches!(
            parse_transition_kind("phase.failed", None),
            Err(HermeticProjectorError::CorruptJournalPayload)
        ));
        assert!(matches!(
            parse_transition_kind("worker.spawned", None),
            Err(HermeticProjectorError::UnrecognizedTransitionKind(_))
        ));
        Ok(())
    }

    #[test]
    fn hard_linked_checkpoint_fails_closed_in_guarded_fallback_read() -> TestResult {
        // Fix 3: `open()`'s final defense-in-depth comparison (and the two
        // no-writer-available fallback receipt paths) now read
        // `checkpoint.json` through `WorkspaceAuthority::verified_lifecycle_checkpoint_bytes`,
        // which checks mode `0600`/exactly-one-link the same way
        // `verify_exact_base` does — not the weaker no-follow-only
        // `lifecycle_checkpoint_bytes`. A hard link raises the file's link
        // count to 2, so the guarded read must fail closed even though the
        // bytes themselves are untouched and a no-follow-only read would
        // have accepted them.
        let home = TestHome::new("hardlink-checkpoint")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        )?;
        projector.close()?;

        let checkpoint_path = home.checkpoint_path(MISSION);
        let hardlink_path = home.workspace_dir(MISSION).join("checkpoint.json.hardlink");
        fs::hard_link(&checkpoint_path, &hardlink_path)?;

        let reopened = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
        );
        assert!(matches!(
            reopened,
            Err(HermeticProjectorError::Workspace(_))
        ));
        Ok(())
    }

    #[test]
    fn recovery_bounds_exhaustion_fails_closed_without_corrupting_anything() -> TestResult {
        // Fix 6: prove `RuntimeStoreError::RecoveryBoundsExceeded` surfaces
        // through `HermeticProjectorError::Store` when a mission's journal
        // exceeds the configured recovery-snapshot bounds, using a
        // `#[cfg(test)]`-only override rather than generating a
        // production-scale (10k-record) journal.
        let home = TestHome::new("recovery-bounds-exhaustion")?;
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let generous_bounds =
            ProjectionRecoveryBounds::new(RECOVERY_MAX_RECORDS, RECOVERY_MAX_BYTES)?;

        let mut projector = HermeticCompatibilityProjector::open_with_recovery_bounds(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
            generous_bounds,
        )?;
        projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;
        let phase = PhaseId::new("phase-1")?;
        projector.apply_transition(
            Some(phase.clone()),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(2),
        )?;
        projector.apply_transition(
            Some(phase),
            ReducerTransition::PhaseCompleted,
            Value::Null,
            Some(passing_verification()),
            ts(3),
        )?;
        projector.apply_transition(
            None,
            ReducerTransition::MissionCompleted,
            Value::Null,
            None,
            ts(4),
        )?;
        projector.close()?;
        let expected_checkpoint_bytes_before = fs::read(home.checkpoint_path(MISSION))?;
        let expected_event_bytes_before = fs::read(home.event_log_path(MISSION))?;

        // Admission + 4 lifecycle transitions = 5 durable records; a bound
        // of 2 must be exceeded.
        let tiny_bounds = ProjectionRecoveryBounds::new(2, RECOVERY_MAX_BYTES)?;
        let too_tight = HermeticCompatibilityProjector::open_with_recovery_bounds(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
            tiny_bounds,
        );
        assert!(matches!(
            too_tight,
            Err(HermeticProjectorError::Store(
                RuntimeStoreError::RecoveryBoundsExceeded
            ))
        ));

        // Nothing was corrupted by the failed attempt: the on-disk artifacts
        // are byte-identical to before it ran, and a subsequent open with
        // adequate bounds succeeds and reaches the correct terminal state.
        assert_eq!(
            fs::read(home.checkpoint_path(MISSION))?,
            expected_checkpoint_bytes_before
        );
        assert_eq!(
            fs::read(home.event_log_path(MISSION))?,
            expected_event_bytes_before
        );
        let generous_bounds_again =
            ProjectionRecoveryBounds::new(RECOVERY_MAX_RECORDS, RECOVERY_MAX_BYTES)?;
        let reopened = HermeticCompatibilityProjector::open_with_recovery_bounds(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
            generous_bounds_again,
        )?;
        assert_eq!(reopened.state().status(), MissionStatus::Completed);
        reopened.close()?;
        Ok(())
    }

    // ---------------------------------------------------------------
    // Subprocess crash campaign (§7.7). Each crash point re-invokes this
    // same unit-test binary as a child process via `current_exe()`, filtered
    // to `hermetic_crash_child` by exact name, exactly as
    // `runtime_store.rs`'s own
    // `committed_wal_recovers_after_process_exit_without_destructors` /
    // `runtime_store_crash_helper` pair does. The child performs real work
    // up to the requested injected boundary and then calls
    // `std::process::exit` directly (no destructors, no WAL checkpoint, no
    // lease release) to simulate a hard crash; the parent then reopens in
    // its own process and asserts the required recovery property.
    // ---------------------------------------------------------------

    fn run_crash_child(home: &TestHome, point: &str) -> TestResult<i32> {
        let output = Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("hermetic_projector::tests::hermetic_crash_child")
            .arg("--nocapture")
            .env(CRASH_HOME_ENV, &home.path)
            .env(CRASH_POINT_ENV, point)
            .output()?;
        Ok(output.status.code().unwrap_or(-1))
    }

    #[test]
    fn hermetic_crash_child() -> TestResult {
        let Some(home_path) = std::env::var_os(CRASH_HOME_ENV) else {
            return Ok(());
        };
        let Some(point) = std::env::var_os(CRASH_POINT_ENV) else {
            return Ok(());
        };
        let point = point.to_string_lossy().into_owned();
        let home_path = fs::canonicalize(home_path)?;
        let boundary = Arc::new(ProductionBoundary::from_canonical_root(&home_path)?);
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mission_markdown = b"# mission\n".to_vec();

        if point == "before_workspace_publish" {
            // Admission step 1-2: append the journal admission record, then
            // exit before the workspace is ever created.
            let mut store =
                RuntimeStore::open(Arc::clone(&boundary), StorageActorAuthority::new())?;
            let pristine_state = build_pristine_state(&mission_id, &plan)?;
            let template = CheckpointProjection {
                workspace_id: mission_id.as_str().to_owned(),
                plan: Some(plan.clone()),
                ..CheckpointProjection::default()
            };
            let initial_checkpoint = checkpoint_for_state(&template, &pristine_state)?;
            let checkpoint_bytes = encode_current_checkpoint(&initial_checkpoint)?;
            let plan_bytes = encode_current_plan(&plan)?;
            let admission_payload = serde_json::json!({
                "mission_sha256": hex_sha256(&mission_markdown),
                "plan_sha256": hex_sha256(&plan_bytes),
            });
            let admission_intent = JournalIntent::new(
                admission_transition_id(&mission_id),
                Some(mission_id.clone()),
                ADMISSION_TRANSITION_KIND,
                admission_payload,
                ts(0),
            )?
            .with_required_projection(CompatibilityProjection::Workspace)
            .with_required_projection(CompatibilityProjection::Checkpoint);
            store.append(&admission_intent)?;
            let _ = checkpoint_bytes;
            std::process::exit(71);
        }

        if point == "after_workspace_publish_before_receipt" {
            // Boundary-2 cut (§7.7): re-inlines exactly the admission prefix
            // `open()` itself runs — append the journal admission record,
            // then durably publish the workspace via `create_production`'s
            // atomic rename (`mission.md`/`plan.json`/`checkpoint.json` all
            // land on disk) — but exit before `ensure_admission_receipts`
            // would ever run, so neither the Workspace nor the Checkpoint
            // receipt is recorded. This is a genuine crash mid-admission,
            // not the no-op re-verification the dead version of this point
            // previously exercised.
            let mut store =
                RuntimeStore::open(Arc::clone(&boundary), StorageActorAuthority::new())?;
            let pristine_state = build_pristine_state(&mission_id, &plan)?;
            let template = CheckpointProjection {
                workspace_id: mission_id.as_str().to_owned(),
                plan: Some(plan.clone()),
                ..CheckpointProjection::default()
            };
            let initial_checkpoint = checkpoint_for_state(&template, &pristine_state)?;
            let plan_bytes = encode_current_plan(&plan)?;
            let admission_payload = serde_json::json!({
                "mission_sha256": hex_sha256(&mission_markdown),
                "plan_sha256": hex_sha256(&plan_bytes),
            });
            let admission_intent = JournalIntent::new(
                admission_transition_id(&mission_id),
                Some(mission_id.clone()),
                ADMISSION_TRANSITION_KIND,
                admission_payload,
                ts(0),
            )?
            .with_required_projection(CompatibilityProjection::Workspace)
            .with_required_projection(CompatibilityProjection::Checkpoint);
            store.append(&admission_intent)?;

            let seed_workspace =
                WorkspaceSeed::new(mission_markdown.clone(), &initial_checkpoint, plan_bytes)?;
            let _workspace_authority = WorkspaceAuthority::create_production(
                Arc::clone(&boundary),
                mission_id.clone(),
                seed_workspace,
            )?;
            std::process::exit(72);
        }

        // Every other crash point starts from a fully admitted mission with
        // its first lifecycle transition (`mission.started`) already fully
        // durable through the ordinary `apply_transition` path. This is the
        // realistic steady state: checkpoint.json no longer reads "pending",
        // so the *second* transition below (`phase.started`) is what gets
        // manually driven up to the requested boundary — only past this
        // first checkpoint advance can the documented
        // `ProductionProjectionWriter` pristine-acquisition gap actually be
        // observed on a fresh reopen.
        let mut projector = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(&mission_markdown, plan),
            ts(0),
        )?;

        if point == "first_transition_after_event_receipt_before_checkpoint_publish" {
            // Contrast case: the mission's very first checkpoint advance
            // (still pristine — checkpoint.json still reads "pending" at
            // this instant) crashes at the identical boundary the later
            // "after_event_receipt_before_checkpoint_publish" point does.
            // Unlike that later point, `ProductionProjectionWriter` can
            // still be re-acquired here on reopen, so recovery succeeds.
            let transition = ReducerTransition::MissionStarted;
            let event_type = event_type_str(&transition);
            let payload = encode_payload(None, &Value::Null, None)?;
            let transition_id = lifecycle_transition_id(&mission_id, None, event_type);
            let intent = JournalIntent::new(
                transition_id,
                Some(mission_id.clone()),
                event_type,
                payload,
                ts(1),
            )?
            .with_required_projection(CompatibilityProjection::EventLog)
            .with_required_projection(CompatibilityProjection::Checkpoint);
            let commit = projector.store.append(&intent)?;
            let recipe = commit
                .event_projection_recipe()
                .ok_or("missing sealed recipe")?
                .clone();
            let event_record = build_event_record(
                &mission_id,
                event_type,
                &ts(1),
                &recipe,
                None,
                &Value::Null,
                None,
            )?;
            let content = encode_current_event(&event_record)?;
            let mut next_line = content.clone();
            next_line.push(b'\n');
            let verified_event = projector
                .event_log
                .publish_exact_next_line(&[], &next_line)?;
            let mut event_jsonl = verified_event.line_bytes().to_vec();
            event_jsonl.push(b'\n');
            let event_receipt =
                ProjectionReceipt::event_log(commit.sequence(), event_jsonl, ts(1))?;
            projector.store.record_projection(&event_receipt)?;
            std::process::exit(79);
        }

        projector.apply_transition(
            None,
            ReducerTransition::MissionStarted,
            Value::Null,
            None,
            ts(1),
        )?;

        let phase = PhaseId::new("phase-1")?;
        let transition = ReducerTransition::PhaseStarted;
        let event_type = event_type_str(&transition);
        let reason = transition_reason(&transition).map(str::to_owned);
        let payload = encode_payload(Some(&phase), &Value::Null, reason.as_deref())?;
        let transition_id = lifecycle_transition_id(&mission_id, Some(&phase), event_type);
        let intent = JournalIntent::new(
            transition_id,
            Some(mission_id.clone()),
            event_type,
            payload,
            ts(2),
        )?
        .with_required_projection(CompatibilityProjection::EventLog)
        .with_required_projection(CompatibilityProjection::Checkpoint);
        let commit = projector.store.append(&intent)?;
        let recipe = commit
            .event_projection_recipe()
            .ok_or("missing sealed recipe")?
            .clone();
        let reducer_input = ReducerInput {
            event_id: EventId::new(recipe.event_id())?,
            sequence: recipe.public_sequence(),
            timestamp: ts(2),
            mission_id: mission_id.clone(),
            phase_id: Some(phase.clone()),
            worker_id: None,
            data: Value::Null,
            extra: BTreeMap::new(),
            transition,
        };
        let reduction = reduce(&projector.state, &reducer_input)?;
        let event_record = build_event_record(
            &mission_id,
            event_type,
            &ts(2),
            &recipe,
            Some(&phase),
            &Value::Null,
            reason.as_deref(),
        )?;
        let target_checkpoint =
            checkpoint_for_state(&projector.checkpoint_template, &reduction.state)?;
        let target_checkpoint_bytes = encode_current_checkpoint(&target_checkpoint)?;

        let mut prior_event_bytes = Vec::new();
        for existing in projector.event_log.events() {
            if existing.record.sequence >= recipe.public_sequence() {
                break;
            }
            prior_event_bytes.extend_from_slice(&existing.raw_line);
        }
        let content = encode_current_event(&event_record)?;
        let separator_required =
            !prior_event_bytes.is_empty() && !prior_event_bytes.ends_with(b"\n");
        let mut next_line = Vec::with_capacity(content.len() + 2);
        if separator_required {
            next_line.push(b'\n');
        }
        next_line.extend_from_slice(&content);
        next_line.push(b'\n');

        if point == "after_journal_before_event_publish" {
            std::process::exit(73);
        }

        let verified_event = projector
            .event_log
            .publish_exact_next_line(&prior_event_bytes, &next_line)?;

        if point == "after_event_rename_before_receipt" {
            std::process::exit(74);
        }

        let mut event_jsonl = verified_event.line_bytes().to_vec();
        event_jsonl.push(b'\n');
        let event_receipt = ProjectionReceipt::event_log(commit.sequence(), event_jsonl, ts(2))?;
        projector.store.record_projection(&event_receipt)?;

        if point == "after_event_receipt_before_checkpoint_publish" {
            std::process::exit(75);
        }

        let writer = projector
            .checkpoint_writer
            .as_ref()
            .ok_or("writer unavailable")?;
        writer.reconcile_checkpoint(&projector.checkpoint_bytes, &target_checkpoint_bytes)?;

        if point == "after_checkpoint_rename_before_receipt" {
            std::process::exit(76);
        }

        let checkpoint_receipt = ProjectionReceipt::compatibility(
            commit.sequence(),
            CompatibilityProjection::Checkpoint,
            ts(2),
        )?;
        projector.store.record_projection(&checkpoint_receipt)?;

        if point == "after_final_receipt_before_response" {
            std::process::exit(77);
        }

        std::process::exit(78);
    }

    #[test]
    fn crash_before_workspace_publish_recovers_via_reopen() -> TestResult {
        let home = TestHome::new("crash-before-workspace")?;
        let code = run_crash_child(&home, "before_workspace_publish")?;
        assert_eq!(code, 71);

        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let reopened = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        )?;
        assert_eq!(reopened.state().status(), MissionStatus::NotStarted);

        // Byte-exact: the durably-published workspace base and checkpoint
        // match exactly what a pristine admission produces, independently
        // re-derived rather than merely status-string-compared.
        let expected_state = build_pristine_state(&mission_id, &plan)?;
        let expected_checkpoint = expected_checkpoint_bytes(MISSION, &plan, &expected_state)?;
        assert_eq!(fs::read(home.mission_md_path(MISSION))?, b"# mission\n");
        assert_eq!(
            fs::read(home.plan_json_path(MISSION))?,
            encode_current_plan(&plan)?
        );
        assert_eq!(
            fs::read(home.checkpoint_path(MISSION))?,
            expected_checkpoint
        );
        assert_eq!(reopened.checkpoint_bytes(), expected_checkpoint.as_slice());
        // Pristine: no lifecycle transition has ever published an event.
        let event_bytes = fs::read(home.event_log_path(MISSION)).unwrap_or_default();
        assert!(event_bytes.is_empty());

        assert_no_temp_residue(&home.path)?;
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn crash_after_workspace_publish_before_receipt_recovers_via_reopen() -> TestResult {
        // Fix 4 (§7.7 boundary-2): the workspace base is durably published
        // (mission.md/plan.json/checkpoint.json all rename-committed) but
        // neither its Workspace nor Checkpoint receipt was recorded before
        // the crash.
        let home = TestHome::new("crash-after-workspace-publish")?;
        let code = run_crash_child(&home, "after_workspace_publish_before_receipt")?;
        assert_eq!(code, 72);

        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let reopened = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        )?;
        assert_eq!(reopened.state().status(), MissionStatus::NotStarted);

        let expected_state = build_pristine_state(&mission_id, &plan)?;
        let expected_checkpoint = expected_checkpoint_bytes(MISSION, &plan, &expected_state)?;
        assert_eq!(fs::read(home.mission_md_path(MISSION))?, b"# mission\n");
        assert_eq!(
            fs::read(home.plan_json_path(MISSION))?,
            encode_current_plan(&plan)?
        );
        assert_eq!(
            fs::read(home.checkpoint_path(MISSION))?,
            expected_checkpoint
        );
        assert_no_temp_residue(&home.path)?;
        reopened.close()?;

        // Ack replay: the receipts minted by the recovering `open()` above
        // are durable, not merely transient in that one process — a THIRD
        // open (a fresh instance, no crash involved) must complete cleanly
        // and idempotently without minting anything new.
        let reopened_again = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
        )?;
        assert_eq!(reopened_again.state().status(), MissionStatus::NotStarted);
        assert_no_temp_residue(&home.path)?;
        reopened_again.close()?;
        Ok(())
    }

    #[test]
    fn crash_before_the_first_checkpoint_advance_still_recovers_via_reopen() -> TestResult {
        // Contrast case for the gap documented below: the mission's very
        // first checkpoint advance is always pristine (checkpoint.json still
        // reads "pending" at the crash instant), so `reconcile_checkpoint`'s
        // writer remains obtainable and recovery completes normally, unlike
        // every later checkpoint advance crashing at the identical boundary.
        let home = TestHome::new("crash-first-transition")?;
        let code = run_crash_child(
            &home,
            "first_transition_after_event_receipt_before_checkpoint_publish",
        )?;
        assert_eq!(code, 79);

        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let reopened = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        )?;
        assert_eq!(reopened.state().status(), MissionStatus::InProgress);

        // Byte-exact event: exactly one `mission.started` event at public
        // sequence 1, addressed to this mission, with no phase attribution.
        let event_bytes = fs::read(home.event_log_path(MISSION))?;
        let scan = scan_event_log(&event_bytes);
        assert!(scan.diagnostics.is_empty());
        assert_eq!(scan.events.len(), 1);
        let only_event = &scan.events[0].record;
        assert_eq!(only_event.event_type, "mission.started");
        assert_eq!(only_event.sequence, 1);
        assert_eq!(only_event.mission_id, MISSION);
        assert_eq!(only_event.phase_id, None);

        // Byte-exact checkpoint: independently re-derived from the expected
        // post-`MissionStarted` reduction, not merely a decoded status
        // string.
        let expected_state = build_pristine_state(&mission_id, &plan)?;
        let started_input = ReducerInput {
            event_id: EventId::new(only_event.id.clone())?,
            sequence: only_event.sequence,
            timestamp: only_event.timestamp.clone(),
            mission_id: mission_id.clone(),
            phase_id: None,
            worker_id: None,
            data: Value::Null,
            extra: BTreeMap::new(),
            transition: ReducerTransition::MissionStarted,
        };
        let expected_state = reduce(&expected_state, &started_input)?.state;
        let expected_checkpoint = expected_checkpoint_bytes(MISSION, &plan, &expected_state)?;
        assert_eq!(
            fs::read(home.checkpoint_path(MISSION))?,
            expected_checkpoint
        );
        assert_eq!(reopened.checkpoint_bytes(), expected_checkpoint.as_slice());

        assert_no_temp_residue(&home.path)?;
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn crash_after_journal_before_event_publish_on_a_non_pristine_mission_recovers_via_reopen()
    -> TestResult {
        // Cell 2E: this used to be one of three fail-closed proofs of the
        // documented `ProductionProjectionWriter` pristine-acquisition gap —
        // checkpoint.json no longer reads "pending" (the first transition
        // already fully durable), so a fresh reopen could not re-acquire a
        // writer via `into_production_projection_writer`. Now
        // `open_body` lazily upgrades to
        // `into_production_projection_writer_recovered` the moment the
        // replay loop actually needs to write a checkpoint, proving its own
        // claim (`checkpoint_bytes_current`) against disk first. The second
        // transition's journal record exists but neither its event nor its
        // checkpoint had been published before the crash — recovery must
        // now complete both, not merely make partial progress.
        let home = TestHome::new("crash-before-event")?;
        let code = run_crash_child(&home, "after_journal_before_event_publish")?;
        assert_eq!(code, 73);

        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let reopened = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        )?;
        assert_eq!(reopened.state().status(), MissionStatus::InProgress);

        // Byte-exact event log: exactly `mission.started` then
        // `phase.started`, with their real on-disk ids/sequences.
        let event_bytes = fs::read(home.event_log_path(MISSION))?;
        let scan = scan_event_log(&event_bytes);
        assert!(scan.diagnostics.is_empty());
        assert_eq!(scan.events.len(), 2);

        // Byte-exact checkpoint: independently re-derived from the on-disk
        // event log, not merely a decoded status string.
        let expected_state = expected_state_after_mission_started_and_phase_started(
            &mission_id,
            &plan,
            &event_bytes,
        )?;
        let expected_checkpoint = expected_checkpoint_bytes(MISSION, &plan, &expected_state)?;
        assert_eq!(
            fs::read(home.checkpoint_path(MISSION))?,
            expected_checkpoint
        );
        assert_eq!(reopened.checkpoint_bytes(), expected_checkpoint.as_slice());

        assert_no_temp_residue(&home.path)?;
        reopened.close()?;

        // Ack replay: the receipts minted by the recovering `open()` above
        // are durable, not merely transient in that one process — a fresh
        // reopen completes cleanly and idempotently, minting nothing new.
        let reopened_again = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
        )?;
        assert_eq!(reopened_again.state().status(), MissionStatus::InProgress);
        assert_eq!(
            reopened_again.checkpoint_bytes(),
            expected_checkpoint.as_slice()
        );
        assert_no_temp_residue(&home.path)?;
        reopened_again.close()?;
        Ok(())
    }

    #[test]
    fn crash_after_event_rename_before_receipt_on_a_non_pristine_mission_recovers_via_reopen()
    -> TestResult {
        // Cell 2E: the second transition's event was already durable on
        // disk before the crash (crossed crash boundary, reconfirmed
        // idempotently on reopen) but unreceipted, and its checkpoint still
        // genuinely needed a write. The lazy upgrade in `open_body` proves
        // `checkpoint_bytes_current` against disk and acquires a recovered
        // writer before this record's checkpoint publish, so both the event
        // receipt and the checkpoint now complete instead of failing closed.
        let home = TestHome::new("crash-after-event-rename")?;
        let code = run_crash_child(&home, "after_event_rename_before_receipt")?;
        assert_eq!(code, 74);

        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let reopened = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        )?;
        assert_eq!(reopened.state().status(), MissionStatus::InProgress);

        let event_bytes = fs::read(home.event_log_path(MISSION))?;
        let scan = scan_event_log(&event_bytes);
        assert!(scan.diagnostics.is_empty());
        assert_eq!(scan.events.len(), 2);

        let expected_state = expected_state_after_mission_started_and_phase_started(
            &mission_id,
            &plan,
            &event_bytes,
        )?;
        let expected_checkpoint = expected_checkpoint_bytes(MISSION, &plan, &expected_state)?;
        assert_eq!(
            fs::read(home.checkpoint_path(MISSION))?,
            expected_checkpoint
        );
        assert_eq!(reopened.checkpoint_bytes(), expected_checkpoint.as_slice());

        assert_no_temp_residue(&home.path)?;
        reopened.close()?;

        let reopened_again = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
        )?;
        assert_eq!(reopened_again.state().status(), MissionStatus::InProgress);
        assert_eq!(
            reopened_again.checkpoint_bytes(),
            expected_checkpoint.as_slice()
        );
        assert_no_temp_residue(&home.path)?;
        reopened_again.close()?;
        Ok(())
    }

    #[test]
    fn crash_after_event_receipt_before_checkpoint_publish_recovers_via_reopen() -> TestResult {
        // Cell 2E: this was the exact scenario
        // `docs/rust-orchestrator/CLAUDE-CELL2-RETURN.md`'s "known
        // fail-closed gap" section named — checkpoint.json already advanced
        // once (from the first transition), so
        // `into_production_projection_writer` could no longer be
        // re-acquired, and the second transition's checkpoint still
        // genuinely needed a write (event already durable and receipted).
        // `into_production_projection_writer_recovered` closes it: the loop
        // proves `checkpoint_bytes_current` (exactly the first transition's
        // target, still on disk here) against disk before writing the
        // second transition's checkpoint.
        let home = TestHome::new("crash-after-event-receipt")?;
        let code = run_crash_child(&home, "after_event_receipt_before_checkpoint_publish")?;
        assert_eq!(code, 75);

        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let reopened = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        )?;
        assert_eq!(reopened.state().status(), MissionStatus::InProgress);

        let event_bytes = fs::read(home.event_log_path(MISSION))?;
        let scan = scan_event_log(&event_bytes);
        assert!(scan.diagnostics.is_empty());
        assert_eq!(scan.events.len(), 2);

        let expected_state = expected_state_after_mission_started_and_phase_started(
            &mission_id,
            &plan,
            &event_bytes,
        )?;
        let expected_checkpoint = expected_checkpoint_bytes(MISSION, &plan, &expected_state)?;
        assert_eq!(
            fs::read(home.checkpoint_path(MISSION))?,
            expected_checkpoint
        );
        assert_eq!(reopened.checkpoint_bytes(), expected_checkpoint.as_slice());

        assert_no_temp_residue(&home.path)?;
        reopened.close()?;

        let reopened_again = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id,
            mission_seed(b"# mission\n", plan),
            ts(0),
        )?;
        assert_eq!(reopened_again.state().status(), MissionStatus::InProgress);
        assert_eq!(
            reopened_again.checkpoint_bytes(),
            expected_checkpoint.as_slice()
        );
        assert_no_temp_residue(&home.path)?;
        reopened_again.close()?;
        Ok(())
    }

    #[test]
    fn crash_after_checkpoint_rename_before_receipt_recovers_via_reopen() -> TestResult {
        let home = TestHome::new("crash-after-checkpoint-rename")?;
        let code = run_crash_child(&home, "after_checkpoint_rename_before_receipt")?;
        assert_eq!(code, 76);

        // The checkpoint write already landed durably before the crash;
        // recovery only needs to mint the missing receipt, which does not
        // require re-acquiring a production projection writer — this
        // succeeds even though the mission is no longer pristine.
        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let reopened = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        )?;
        assert_eq!(reopened.state().status(), MissionStatus::InProgress);

        // Byte-exact: re-derive the expected post-`PhaseStarted` state from
        // the real, on-disk event log and compare full checkpoint bytes.
        let event_bytes = fs::read(home.event_log_path(MISSION))?;
        let expected_state = expected_state_after_mission_started_and_phase_started(
            &mission_id,
            &plan,
            &event_bytes,
        )?;
        let expected_checkpoint = expected_checkpoint_bytes(MISSION, &plan, &expected_state)?;
        assert_eq!(
            fs::read(home.checkpoint_path(MISSION))?,
            expected_checkpoint
        );
        assert_eq!(reopened.checkpoint_bytes(), expected_checkpoint.as_slice());

        assert_no_temp_residue(&home.path)?;
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn crash_after_final_receipt_before_response_replays_the_acknowledgement() -> TestResult {
        let home = TestHome::new("crash-after-final-receipt")?;
        let code = run_crash_child(&home, "after_final_receipt_before_response")?;
        assert_eq!(code, 77);

        let boundary = home.boundary()?;
        let mission_id = MissionId::new(MISSION)?;
        let plan = simple_plan(&["phase-1"]);
        let mut reopened = HermeticCompatibilityProjector::open(
            Arc::clone(&boundary),
            mission_id.clone(),
            mission_seed(b"# mission\n", plan.clone()),
            ts(0),
        )?;
        assert_eq!(reopened.state().status(), MissionStatus::InProgress);

        // Byte-exact, pre-replay: the recovered checkpoint already matches
        // the expected post-`PhaseStarted` state re-derived from the real
        // on-disk event log.
        let event_bytes_before = fs::read(home.event_log_path(MISSION))?;
        let expected_state = expected_state_after_mission_started_and_phase_started(
            &mission_id,
            &plan,
            &event_bytes_before,
        )?;
        let expected_checkpoint = expected_checkpoint_bytes(MISSION, &plan, &expected_state)?;
        assert_eq!(reopened.checkpoint_bytes(), expected_checkpoint.as_slice());

        // Replaying the same (second) transition again must be a pure
        // acknowledgement replay: no second event, no second checkpoint
        // write — byte-identical before and after the replay.
        let phase = PhaseId::new("phase-1")?;
        let outcome = reopened.apply_transition(
            Some(phase),
            ReducerTransition::PhaseStarted,
            Value::Null,
            None,
            ts(2),
        )?;
        assert!(outcome.replayed);
        let event_bytes_after = fs::read(home.event_log_path(MISSION))?;
        assert_eq!(event_bytes_after, event_bytes_before);
        let scan = scan_event_log(&event_bytes_after);
        assert_eq!(scan.events.len(), 2);
        assert_eq!(
            fs::read(home.checkpoint_path(MISSION))?,
            expected_checkpoint
        );
        assert_eq!(reopened.checkpoint_bytes(), expected_checkpoint.as_slice());

        assert_no_temp_residue(&home.path)?;
        reopened.close()?;
        Ok(())
    }
}
