//! Mission reasoning and dispatch composition.
//!
//! [`MissionService`] flows an untrusted proposal through **compile → seed →
//! persist → dispatch** over the R0/B1-safe storage actor and the B1 executor
//! registry. Compilation ([`compile_proposal`]) is a hard precondition and *is*
//! the DAG validation: a malformed or cyclic proposal returns an error before
//! anything is persisted or dispatched, so such a plan can never leave a
//! reasoning row or reach an executor.
//!
//! Reasoning state is Rust-private and lives in a private-process-ledger store,
//! never in the Go-visible compatibility projections. It is written only
//! through the typed [`ReasoningWrite`] boundary, which has no free-form or
//! raw-text field — provider chain-of-thought is therefore structurally
//! unrepresentable, not filtered after the fact. In particular, a terminal
//! [`AttemptOutcome`]'s partial output is deliberately never mapped into any
//! reasoning field.

use std::collections::BTreeMap;
use std::sync::Arc;

use orchestrator_core::{
    AuthoredParseContext, AuthoredPhase, CompiledPlan, ExecutionMode, MissionId, MissionProposal,
    MissionState, MissionStateBuildError, PhaseDefinition, PhaseId, ProposalError,
    compile_proposal,
};
use orchestrator_exec::{
    AttemptOutcome, DispatchError, ExecutionContext, ExecutionRequest, ExecutorRegistry,
    MechanicalTermination, RuntimeFamily, RuntimeRegistryError, SessionHandle,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::continuation::{
    ContinuationCapsule, ContinuationDecision, ContinuationDecisionSummary, ContinuationError,
    select_resume_or_fresh, strategy_fingerprint,
};

use crate::runtime_home::ProductionBoundary;
use crate::runtime_store::{
    ContinuationDecisionKind, ContinuationDecisionRecord, ContinuationHandleRecord,
    PrivateProcessLedgerBoundary, PrivateProcessLedgerStore, ReasoningAssignmentRow,
    ReasoningAttempt, ReasoningCounts, ReasoningCriterionRow, ReasoningEvidenceRow,
    ReasoningHandoffRow, ReasoningIntent, ReasoningReviewRow, ReasoningRevision, ReasoningWrite,
    RuntimeStore, RuntimeStoreError, StorageActorAuthority, hex_digest,
};

/// Longest bounded reasoning text mapped from a plan objective or summary.
const MAX_REASONING_SUMMARY_BYTES: usize = 1024;

/// A validated, admitted mission and its reduction seed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedMission {
    mission_id: MissionId,
    plan: CompiledPlan,
    state: MissionState,
}

impl AdmittedMission {
    /// The mission identifier.
    #[must_use]
    pub const fn mission_id(&self) -> &MissionId {
        &self.mission_id
    }

    /// The compiled, dispatchable plan.
    #[must_use]
    pub const fn plan(&self) -> &CompiledPlan {
        &self.plan
    }

    /// The reduction seed built from the compiled DAG.
    #[must_use]
    pub const fn state(&self) -> &MissionState {
        &self.state
    }

    fn phase(&self, phase_id: &PhaseId) -> Option<&AuthoredPhase> {
        self.plan.phases.iter().find(|phase| &phase.id == phase_id)
    }
}

/// Structured failures of the mission service.
#[derive(Debug, Error)]
pub enum MissionServiceError {
    /// The proposal could not be compiled into a dependency DAG.
    #[error("proposal did not compile: {0}")]
    Proposal(#[from] ProposalError),
    /// The compiled DAG failed to seed reduced state (should not occur once
    /// [`compile_proposal`] has accepted the proposal).
    #[error("compiled plan did not seed mission state: {0}")]
    MissionState(#[from] MissionStateBuildError),
    /// The private reasoning store rejected a write.
    #[error("reasoning store operation failed: {0}")]
    Store(#[from] RuntimeStoreError),
    /// The requested runtime could not be resolved.
    #[error("runtime resolution failed: {0}")]
    Registry(#[from] RuntimeRegistryError),
    /// Dispatch could not be bound before executor entry.
    #[error("dispatch binding failed: {0}")]
    Dispatch(#[from] DispatchError),
    /// A phase id was not present in the admitted plan.
    #[error("phase {0} is not part of the admitted mission")]
    UnknownPhase(String),
    /// A continuation capsule or decision could not be built.
    #[error("continuation failed: {0}")]
    Continuation(#[from] ContinuationError),
}

/// Composes proposal compilation, reasoning persistence, and one-phase dispatch
/// over a single-writer private reasoning store and the executor registry.
pub struct MissionService {
    store: PrivateProcessLedgerStore,
}

impl MissionService {
    /// Opens (or initializes) the Rust-private reasoning store on an injected,
    /// already-enrolled production boundary.
    ///
    /// The boundary is supplied by the composition layer that owns runtime-home
    /// enrollment (constructor injection); the store is an isolated
    /// private-process-ledger database, so its reasoning tables are never part
    /// of the Go-visible compatibility projections. This entry point is
    /// composition-root-only until production runtime-home enrollment lands: no
    /// out-of-crate caller can mint a [`ProductionBoundary`] today.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError::Store`] if the private ledger cannot be
    /// opened.
    pub fn open(boundary: Arc<ProductionBoundary>) -> Result<Self, MissionServiceError> {
        let store = RuntimeStore::open_private(
            PrivateProcessLedgerBoundary::new(boundary),
            StorageActorAuthority::new(),
        )?;
        Ok(Self { store })
    }

    /// Closes the reasoning store, flushing and releasing its writer lease.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError::Store`] on a flush/close failure.
    pub fn close(self) -> Result<(), MissionServiceError> {
        self.store.close()?;
        Ok(())
    }

    /// Compiles a proposal into a typed DAG, seeds reduced state, and persists
    /// the mission's reasoning record — atomically and before any dispatch.
    ///
    /// A [`ProposalError`] (malformed, cyclic, unknown/self dependency, …)
    /// returns here having persisted nothing and dispatched nothing.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError`] on a compile failure, a seed failure, or
    /// a store failure. On any error, no reasoning row exists for the mission.
    pub fn admit_mission(
        &mut self,
        mission_id: MissionId,
        proposal: &MissionProposal,
        parse_context: &AuthoredParseContext,
        committed_at_utc: &str,
    ) -> Result<AdmittedMission, MissionServiceError> {
        let plan = compile_proposal(proposal, parse_context)?;
        let definitions: Vec<PhaseDefinition> = plan
            .phases
            .iter()
            .map(|phase| PhaseDefinition {
                id: phase.id.clone(),
                dependencies: phase.dependencies.clone(),
            })
            .collect();
        let state = MissionState::new(mission_id.clone(), definitions)?;
        let intent = build_intent(&mission_id, &plan, committed_at_utc);
        self.store
            .record_reasoning(&ReasoningWrite::Intent(intent))?;
        Ok(AdmittedMission {
            mission_id,
            plan,
            state,
        })
    }

    /// Records a DAG/plan revision reason for an admitted mission.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError::Store`] on a write failure.
    pub fn record_revision(
        &mut self,
        mission: &AdmittedMission,
        revision: u32,
        reason: &str,
        committed_at_utc: &str,
    ) -> Result<(), MissionServiceError> {
        self.store
            .record_reasoning(&ReasoningWrite::Revision(ReasoningRevision {
                mission_id: mission.mission_id.clone(),
                committed_at_utc: committed_at_utc.to_owned(),
                revision,
                reason: bounded(reason),
            }))?;
        Ok(())
    }

    /// Maps a terminal [`AttemptOutcome`] to a typed reasoning attempt record.
    ///
    /// The outcome's raw/partial output is deliberately not mapped into any
    /// field — only typed evidence (a strategy fingerprint, an outcome tag, and
    /// a failure-kind tag) is persisted.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError`] if the phase is unknown to the mission or
    /// the store rejects the write.
    pub fn record_phase_attempt(
        &mut self,
        mission: &AdmittedMission,
        phase_id: &PhaseId,
        attempt: u32,
        revision: u32,
        outcome: &AttemptOutcome,
        committed_at_utc: &str,
    ) -> Result<(), MissionServiceError> {
        let Some(phase) = mission.phase(phase_id) else {
            return Err(MissionServiceError::UnknownPhase(
                phase_id.as_str().to_owned(),
            ));
        };
        let outcome_tag = if outcome.is_completed() {
            "completed"
        } else {
            "incomplete"
        };
        let failure_kind = outcome
            .termination()
            .map(|termination| termination_tag(termination).to_owned());
        // Derive the fingerprint from the strategy INPUTS (the phase's resolved
        // assignment plus the plan revision), never the attempt ordinal. Folding
        // in `attempt` made every retry unique by construction, so an unchanged
        // strategy could never be detected; deriving from the inputs means a
        // cosmetic retry collides (rejected upstream) while a materially
        // different retry — an escalated tier/model/persona or a bumped revision
        // — yields a fresh fingerprint.
        let fingerprint = strategy_fingerprint(
            &mission.mission_id,
            phase_id,
            revision,
            &phase.runtime,
            &phase.model_tier,
            &phase.persona,
            &phase.role,
        );
        self.store
            .record_reasoning(&ReasoningWrite::Attempt(ReasoningAttempt {
                mission_id: mission.mission_id.clone(),
                committed_at_utc: committed_at_utc.to_owned(),
                phase_id: phase_id.clone(),
                attempt,
                strategy_fingerprint: fingerprint,
                outcome: outcome_tag.to_owned(),
                failure_kind,
            }))?;
        Ok(())
    }

    /// Dispatches exactly one phase through the enforced registry seam and
    /// records the resulting attempt as typed reasoning.
    ///
    /// The registry is the sole producer of the underlying dispatch request;
    /// this method only resolves the runtime, drives the enforced
    /// `ResolvedExecutor::execute`, and maps the terminal outcome.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError`] on an unknown phase, a runtime-resolution
    /// failure, a pre-dispatch binding fault, or a reasoning write failure.
    pub fn run_one_phase(
        &mut self,
        mission: &AdmittedMission,
        phase_id: &PhaseId,
        request: &ExecutionRequest,
        registry: &ExecutorRegistry,
        context: &mut ExecutionContext<'_>,
        committed_at_utc: &str,
    ) -> Result<AttemptOutcome, MissionServiceError> {
        if mission.phase(phase_id).is_none() {
            return Err(MissionServiceError::UnknownPhase(
                phase_id.as_str().to_owned(),
            ));
        }
        let resolved = registry.resolve(request.runtime().as_str())?;
        let outcome = resolved.execute(request, context)?;
        self.record_phase_attempt(
            mission,
            phase_id,
            request.attempt(),
            request.revision(),
            &outcome,
            committed_at_utc,
        )?;
        Ok(outcome)
    }

    /// Reasoning projection row counts for a mission.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError::Store`] on a read failure.
    pub fn reasoning_counts(
        &self,
        mission_id: &MissionId,
    ) -> Result<ReasoningCounts, MissionServiceError> {
        Ok(self.store.reasoning_counts(mission_id)?)
    }

    /// Every `(table, column)` across the reasoning projection tables, for
    /// asserting no chain-of-thought / raw-output column exists.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError::Store`] on a read failure.
    pub fn reasoning_column_inventory(&self) -> Result<Vec<(String, String)>, MissionServiceError> {
        Ok(self.store.reasoning_column_inventory()?)
    }

    /// Every persisted reasoning text cell for a mission, for asserting no raw
    /// provider output leaked into reasoning state.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError::Store`] on a read failure.
    pub fn reasoning_text_cells(
        &self,
        mission_id: &MissionId,
    ) -> Result<Vec<String>, MissionServiceError> {
        Ok(self.store.reasoning_text_cells(mission_id)?)
    }

    /// The prior strategy fingerprints recorded for a phase, in attempt order.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError::Store`] on a read failure.
    pub fn phase_strategy_fingerprints(
        &self,
        mission_id: &MissionId,
        phase_id: &PhaseId,
    ) -> Result<Vec<String>, MissionServiceError> {
        Ok(self
            .store
            .reasoning_attempt_fingerprints(mission_id, phase_id.as_str())?)
    }

    /// Builds the provider-neutral capsule that would seed the phase's next
    /// attempt, folding in the strategies already tried so a retry cannot repeat
    /// one.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError`] if the phase is unknown, an ordinal is
    /// zero, the runtime family does not parse, or the store read fails.
    pub fn build_continuation_capsule(
        &self,
        mission: &AdmittedMission,
        phase_id: &PhaseId,
        attempt: u32,
        revision: u32,
    ) -> Result<ContinuationCapsule, MissionServiceError> {
        let prior = self.phase_strategy_fingerprints(mission.mission_id(), phase_id)?;
        Ok(ContinuationCapsule::from_admitted(
            mission, phase_id, attempt, revision, prior,
        )?)
    }

    /// Durably records a same-provider continuation handle for one exact
    /// mission/phase/attempt binding. Exact retries are idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError::Store`] on a write failure.
    pub fn record_continuation_handle(
        &mut self,
        mission: &AdmittedMission,
        phase_id: &PhaseId,
        attempt: u32,
        handle: &SessionHandle,
    ) -> Result<(), MissionServiceError> {
        let record = ContinuationHandleRecord::from_handle(
            mission.mission_id(),
            phase_id.as_str(),
            attempt,
            handle,
        )?;
        self.store.record_continuation_handle(&record)?;
        Ok(())
    }

    /// The durable same-provider handle for one exact mission/phase/attempt
    /// binding, if one was recorded.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError::Store`] on a read failure.
    pub fn continuation_handle(
        &self,
        mission_id: &MissionId,
        phase_id: &PhaseId,
        attempt: u32,
    ) -> Result<Option<SessionHandle>, MissionServiceError> {
        match self
            .store
            .continuation_handle(mission_id, phase_id.as_str(), attempt)?
        {
            Some(record) => Ok(Some(record.to_session_handle()?)),
            None => Ok(None),
        }
    }

    /// Durably records the selected continuation decision for one exact
    /// mission/phase/attempt binding. Exact retries are idempotent; a divergent
    /// rewrite conflicts, so an accepted decision can never be silently changed
    /// on resume.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError::Store`] on a write failure.
    pub fn record_continuation_decision(
        &mut self,
        mission_id: &MissionId,
        phase_id: &PhaseId,
        attempt: u32,
        revision: u32,
        decision: &ContinuationDecision,
    ) -> Result<(), MissionServiceError> {
        let record = match decision {
            ContinuationDecision::ResumeSameProvider { handle } => ContinuationDecisionRecord::new(
                mission_id,
                phase_id.as_str(),
                attempt,
                revision,
                ContinuationDecisionKind::ResumeSameProvider,
                handle.runtime_family().as_str(),
                None,
            )?,
            ContinuationDecision::FreshFromCapsule { capsule } => ContinuationDecisionRecord::new(
                mission_id,
                phase_id.as_str(),
                attempt,
                revision,
                ContinuationDecisionKind::FreshFromCapsule,
                capsule.runtime().as_str(),
                Some(capsule.canonical_digest()),
            )?,
        };
        self.store.record_continuation_decision(&record)?;
        Ok(())
    }

    /// Selects same-provider resume vs a fresh cross-provider session for the
    /// phase's next attempt.
    ///
    /// Deterministic and offline: it consults the durable handle recorded for
    /// the immediately prior attempt and the resolved next runtime, then applies
    /// [`select_resume_or_fresh`]. A durable terminal decision or already-durable
    /// receipt for the prior attempt is never re-emitted — the accepted effect
    /// is consulted, not repeated.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError`] on an unknown phase, a corrupt handle, or
    /// a store failure.
    pub fn select_continuation(
        &self,
        mission: &AdmittedMission,
        phase_id: &PhaseId,
        next_attempt: u32,
        revision: u32,
        next_runtime: &RuntimeFamily,
    ) -> Result<ContinuationDecision, MissionServiceError> {
        let prior_handle = if next_attempt > 1 {
            self.continuation_handle(mission.mission_id(), phase_id, next_attempt - 1)?
        } else {
            None
        };
        let capsule = self.build_continuation_capsule(mission, phase_id, next_attempt, revision)?;
        Ok(select_resume_or_fresh(prior_handle, next_runtime, capsule))
    }

    /// Every persisted continuation decision payload for a mission, for
    /// asserting no raw provider output or provider `session_id` leaked into a
    /// provider-neutral decision row. Also the byte-identical replay surface.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError::Store`] on a read failure.
    pub fn continuation_capsule_text_cells(
        &self,
        mission_id: &MissionId,
    ) -> Result<Vec<String>, MissionServiceError> {
        Ok(self.store.continuation_capsule_text_cells(mission_id)?)
    }

    /// The sorted set of distinct top-level payload keys across a mission's
    /// continuation decision rows, for asserting no free-form / secret-bearing
    /// key exists.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError::Store`] on a read failure.
    pub fn continuation_decision_key_inventory(
        &self,
        mission_id: &MissionId,
    ) -> Result<Vec<String>, MissionServiceError> {
        Ok(self.store.continuation_decision_key_inventory(mission_id)?)
    }

    /// Every recorded continuation decision for a mission, in ascending attempt
    /// order, as replay-stable summaries. Re-deriving the same decisions over
    /// these durable rows reproduces the original resume-vs-fresh choices
    /// offline, without re-running any provider.
    ///
    /// # Errors
    ///
    /// Returns [`MissionServiceError::Store`] on a read failure.
    pub fn continuation_decisions(
        &self,
        mission_id: &MissionId,
    ) -> Result<Vec<ContinuationDecisionSummary>, MissionServiceError> {
        Ok(self
            .store
            .continuation_decisions(mission_id)?
            .iter()
            .map(|record| record.summary())
            .collect())
    }
}

/// Builds the typed admission reasoning record from a compiled plan.
///
/// Every field is plan-derived and bounded; there is no path for provider text.
fn build_intent(
    mission_id: &MissionId,
    plan: &CompiledPlan,
    committed_at_utc: &str,
) -> ReasoningIntent {
    let mode = match plan.execution_mode {
        ExecutionMode::Sequential => "sequential",
        ExecutionMode::Parallel => "parallel",
    };
    let phase_names: BTreeMap<&PhaseId, &str> = plan
        .phases
        .iter()
        .map(|phase| (&phase.id, phase.name.as_str()))
        .collect();
    let mut criteria = Vec::new();
    let mut evidence = Vec::new();
    let mut assignments = Vec::new();
    let mut reviews = Vec::new();
    let mut handoffs = Vec::new();
    for phase in &plan.phases {
        criteria.push(ReasoningCriterionRow {
            phase_id: phase.id.clone(),
            criterion: bounded(&phase.objective),
        });
        evidence.push(ReasoningEvidenceRow {
            phase_id: phase.id.clone(),
            kind: "objective".to_owned(),
            digest: sha256_hex(&phase.objective),
            summary: bounded(&phase.objective),
        });
        assignments.push(ReasoningAssignmentRow {
            phase_id: phase.id.clone(),
            persona: bounded(&phase.persona),
            role: bounded(&phase.role),
            model_tier: bounded(&phase.model_tier),
            runtime: bounded(&phase.runtime),
            selection_method: bounded(&phase.persona_selection_method),
        });
        reviews.push(ReasoningReviewRow {
            phase_id: phase.id.clone(),
            verdict: "pending".to_owned(),
            reviewer_persona: bounded(&phase.persona),
        });
        for dependency in &phase.dependencies {
            let from_name = phase_names
                .get(dependency)
                .copied()
                .unwrap_or_else(|| dependency.as_str());
            handoffs.push(ReasoningHandoffRow {
                from_phase: dependency.clone(),
                to_phase: phase.id.clone(),
                summary: bounded(&format!("{from_name} feeds {}", phase.name)),
            });
        }
    }
    ReasoningIntent {
        mission_id: mission_id.clone(),
        committed_at_utc: committed_at_utc.to_owned(),
        intent: bounded(&format!(
            "compiled {}-phase mission ({mode} mode)",
            plan.phases.len()
        )),
        assumptions: vec![
            "plan compiled deterministically from the proposal".to_owned(),
            format!("{mode} execution mode"),
        ],
        criteria,
        evidence,
        assignments,
        handoffs,
        reviews,
    }
}

/// Truncates text to a bounded, char-boundary-safe reasoning field.
fn bounded(value: &str) -> String {
    if value.len() <= MAX_REASONING_SUMMARY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_REASONING_SUMMARY_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// Lowercase hex SHA-256 of the input, reusing the store's hex encoder.
fn sha256_hex(input: &str) -> String {
    hex_digest(Sha256::digest(input.as_bytes()).as_slice())
}

/// Stable tag for a mechanical termination, carrying no free-form detail.
const fn termination_tag(termination: MechanicalTermination) -> &'static str {
    match termination {
        MechanicalTermination::Cancelled => "cancelled",
        MechanicalTermination::HardDeadlineExceeded => "hard_deadline_exceeded",
        MechanicalTermination::WatchdogStalled => "watchdog_stalled",
        MechanicalTermination::ProcessExited(_) => "process_exited",
        MechanicalTermination::ProviderStreamEnded => "provider_stream_ended",
        MechanicalTermination::SupervisorFailure => "supervisor_failure",
        MechanicalTermination::EventDeliveryFailure => "event_delivery_failure",
        MechanicalTermination::ContractViolation => "contract_violation",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use std::path::PathBuf;

    use orchestrator_core::{ProposedPhase, keyword_fallback_proposal};
    use orchestrator_exec::{
        AttemptEvidence, ContractError, Effort, ExecutionRequestDraft, MechanicalTermination,
        PartialWork, RuntimeFamily, SessionHandle,
    };

    use crate::continuation::{ContinuationDecision, RetryPlan, select_retry};

    use crate::runtime_home::ProductionBoundary;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    static NEXT: AtomicU64 = AtomicU64::new(1);

    /// A throwaway private home whose directory is removed on drop.
    struct TestHome {
        path: std::path::PathBuf,
    }

    impl TestHome {
        fn service(&self) -> Result<MissionService, Box<dyn std::error::Error>> {
            let boundary = Arc::new(ProductionBoundary::from_canonical_root(&self.path)?);
            Ok(MissionService::open(boundary)?)
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn open_home() -> Result<TestHome, Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!(
            "nanika-mission-service-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        }
        let path = std::fs::canonicalize(path)?;
        Ok(TestHome { path })
    }

    fn proposed(name: &str, objective: &str, persona: &str, deps: &[&str]) -> ProposedPhase {
        ProposedPhase {
            name: name.to_owned(),
            objective: objective.to_owned(),
            persona: persona.to_owned(),
            skills: Vec::new(),
            depends_on: deps
                .iter()
                .map(|dependency| (*dependency).to_owned())
                .collect(),
        }
    }

    fn valid_proposal() -> MissionProposal {
        MissionProposal {
            phases: vec![
                proposed("plan", "produce a build plan", "architect", &[]),
                proposed(
                    "build",
                    "implement the plan",
                    "senior-backend-engineer",
                    &["plan"],
                ),
            ],
        }
    }

    #[test]
    fn admit_persists_reasoning_projection_rows() -> TestResult {
        let home = open_home()?;
        let mut service = home.service()?;
        let mission_id = MissionId::new("mission-admit")?;
        let admitted = service.admit_mission(
            mission_id.clone(),
            &valid_proposal(),
            &AuthoredParseContext::default(),
            "2026-07-24T00:00:00Z",
        )?;
        assert_eq!(admitted.plan().phases.len(), 2);
        let counts = service.reasoning_counts(&mission_id)?;
        assert_eq!(counts.records, 1);
        assert_eq!(counts.criteria, 2);
        assert_eq!(counts.evidence, 2);
        assert_eq!(counts.assignments, 2);
        assert_eq!(counts.reviews, 2);
        assert_eq!(counts.handoffs, 1);
        assert!(counts.assumptions >= 1);
        service.close()?;
        Ok(())
    }

    #[test]
    fn revision_is_recorded() -> TestResult {
        let home = open_home()?;
        let mut service = home.service()?;
        let mission_id = MissionId::new("mission-revision")?;
        let admitted = service.admit_mission(
            mission_id.clone(),
            &valid_proposal(),
            &AuthoredParseContext::default(),
            "2026-07-24T00:00:00Z",
        )?;
        service.record_revision(&admitted, 1, "reordered phases", "2026-07-24T00:00:01Z")?;
        assert_eq!(service.reasoning_counts(&mission_id)?.revisions, 1);
        service.close()?;
        Ok(())
    }

    #[test]
    fn attempt_never_persists_provider_chain_of_thought() -> TestResult {
        const SECRET: &str = "SECRET-CHAIN-OF-THOUGHT-do-not-persist";
        let home = open_home()?;
        let mut service = home.service()?;
        let mission_id = MissionId::new("mission-cot")?;
        let admitted = service.admit_mission(
            mission_id.clone(),
            &valid_proposal(),
            &AuthoredParseContext::default(),
            "2026-07-24T00:00:00Z",
        )?;
        let phase_id = admitted.plan().phases[0].id.clone();
        let outcome = AttemptOutcome::incomplete(
            MechanicalTermination::ProviderStreamEnded,
            None,
            PartialWork::new(Some(SECRET.to_owned()), AttemptEvidence::new())?,
            Duration::from_millis(1),
        );
        service.record_phase_attempt(
            &admitted,
            &phase_id,
            1,
            1,
            &outcome,
            "2026-07-24T00:00:02Z",
        )?;
        assert_eq!(service.reasoning_counts(&mission_id)?.attempts, 1);

        // (a) No reasoning column is named like chain-of-thought / raw output.
        let forbidden = [
            "chain_of_thought",
            "cot",
            "raw",
            "provider_text",
            "reasoning_text",
        ];
        for (table, column) in service.reasoning_column_inventory()? {
            let lowered = column.to_ascii_lowercase();
            for needle in forbidden {
                assert!(
                    !lowered.contains(needle),
                    "reasoning table {table} exposes a {needle}-like column: {column}"
                );
            }
        }
        // (c) The provider's partial output never appears in any reasoning cell.
        for cell in service.reasoning_text_cells(&mission_id)? {
            assert!(
                !cell.contains(SECRET),
                "provider chain-of-thought leaked into a reasoning row"
            );
        }
        service.close()?;
        Ok(())
    }

    #[test]
    fn cyclic_proposal_persists_nothing() -> TestResult {
        let home = open_home()?;
        let mut service = home.service()?;
        let mission_id = MissionId::new("mission-cyclic")?;
        let cyclic = MissionProposal {
            phases: vec![
                proposed("a", "first", "p", &["b"]),
                proposed("b", "second", "p", &["a"]),
            ],
        };
        let result = service.admit_mission(
            mission_id.clone(),
            &cyclic,
            &AuthoredParseContext::default(),
            "2026-07-24T00:00:00Z",
        );
        assert!(matches!(
            result,
            Err(MissionServiceError::Proposal(ProposalError::Cycle { .. }))
        ));
        assert!(service.reasoning_counts(&mission_id)?.is_empty());
        service.close()?;
        Ok(())
    }

    #[test]
    fn malformed_proposals_persist_nothing() -> TestResult {
        let home = open_home()?;
        let mut service = home.service()?;
        let context = AuthoredParseContext::default();

        let empty = MissionProposal::default();
        let empty_result = service.admit_mission(
            MissionId::new("mission-empty")?,
            &empty,
            &context,
            "2026-07-24T00:00:00Z",
        );
        assert!(matches!(
            empty_result,
            Err(MissionServiceError::Proposal(ProposalError::Empty))
        ));

        let unknown = MissionProposal {
            phases: vec![proposed("only", "x", "p", &["ghost"])],
        };
        let unknown_result = service.admit_mission(
            MissionId::new("mission-unknown")?,
            &unknown,
            &context,
            "2026-07-24T00:00:00Z",
        );
        assert!(matches!(
            unknown_result,
            Err(MissionServiceError::Proposal(
                ProposalError::UnknownDependency { .. }
            ))
        ));

        assert!(
            service
                .reasoning_counts(&MissionId::new("mission-empty")?)?
                .is_empty()
        );
        assert!(
            service
                .reasoning_counts(&MissionId::new("mission-unknown")?)?
                .is_empty()
        );
        service.close()?;
        Ok(())
    }

    #[test]
    fn continuation_capsule_never_carries_seeded_provider_secret() -> TestResult {
        const SECRET: &str = "SECRET-PROVIDER-OUTPUT-do-not-carry";
        let home = open_home()?;
        let mut service = home.service()?;
        let mission_id = MissionId::new("mission-capsule-secret")?;
        let admitted = service.admit_mission(
            mission_id.clone(),
            &valid_proposal(),
            &AuthoredParseContext::default(),
            "2026-07-24T00:00:00Z",
        )?;
        let phase_id = admitted.plan().phases[0].id.clone();
        // A failed attempt whose partial output carries a provider secret. The
        // secret is dropped at record time and never reaches any reasoning row.
        let outcome = AttemptOutcome::incomplete(
            MechanicalTermination::ProviderStreamEnded,
            None,
            PartialWork::new(Some(SECRET.to_owned()), AttemptEvidence::new())?,
            Duration::from_millis(1),
        );
        service.record_phase_attempt(
            &admitted,
            &phase_id,
            1,
            1,
            &outcome,
            "2026-07-24T00:00:01Z",
        )?;

        // The capsule for the next attempt is built purely from the plan; the
        // provider secret is unrepresentable in it, by construction.
        let capsule = service.build_continuation_capsule(&admitted, &phase_id, 2, 1)?;
        for field in capsule.text_fields() {
            assert!(
                !field.contains(SECRET),
                "capsule field leaked provider output"
            );
        }
        // The prior strategy fingerprint the capsule folds in is a hash, not text.
        assert_eq!(capsule.prior_strategy_fingerprints().len(), 1);

        // And nothing secret is ever persisted to a continuation decision row.
        let decision = ContinuationDecision::FreshFromCapsule {
            capsule: Box::new(capsule),
        };
        service.record_continuation_decision(&mission_id, &phase_id, 2, 1, &decision)?;
        for cell in service.continuation_capsule_text_cells(&mission_id)? {
            assert!(
                !cell.contains(SECRET),
                "continuation row leaked provider output"
            );
        }
        service.close()?;
        Ok(())
    }

    #[test]
    fn cross_family_handle_reuse_is_rejected_fail_closed() -> TestResult {
        let claude = RuntimeFamily::parse("claude")?;
        let codex = RuntimeFamily::parse("codex")?;
        let handle = SessionHandle::new(claude.clone(), "provider-session")?;
        // Layer 1: the handle itself refuses to resume into a different family.
        assert!(handle.can_resume_into(&claude));
        assert!(!handle.can_resume_into(&codex));

        // Layer 2: a request that pairs a claude handle with a codex runtime is
        // unconstructable.
        let draft = ExecutionRequestDraft {
            mission: "mission".to_owned(),
            phase: "phase".to_owned(),
            attempt: 2,
            revision: 1,
            objective: "resume work".to_owned(),
            persona: "architect".to_owned(),
            role: "planner".to_owned(),
            domain: "dev".to_owned(),
            skills: Vec::new(),
            dependencies: Vec::new(),
            expected_evidence: Vec::new(),
            constraints: Vec::new(),
            prior_context: String::new(),
            runtime: codex.clone(),
            model: "some-model".to_owned(),
            effort: Effort::High,
            max_turns: 0,
            worker_dir: PathBuf::from("/tmp/continuation-worker"),
            target_dir: None,
            resume_from: Some(handle.clone()),
            hook_script: None,
        };
        assert!(matches!(
            ExecutionRequest::new(draft),
            Err(ContractError::SessionRuntimeMismatch)
        ));

        // Layer 3: the selector never resumes cross-family — it falls through to
        // a fresh capsule even though a same-mission handle is durable.
        let home = open_home()?;
        let mut service = home.service()?;
        let mission_id = MissionId::new("mission-cross-family")?;
        let admitted = service.admit_mission(
            mission_id.clone(),
            &valid_proposal(),
            &AuthoredParseContext::default(),
            "2026-07-24T00:00:00Z",
        )?;
        let phase_id = admitted.plan().phases[0].id.clone();
        service.record_continuation_handle(&admitted, &phase_id, 1, &handle)?;
        let decision = service.select_continuation(&admitted, &phase_id, 2, 1, &codex)?;
        assert!(matches!(
            decision,
            ContinuationDecision::FreshFromCapsule { .. }
        ));
        service.close()?;
        Ok(())
    }

    #[test]
    fn resume_selected_only_when_handle_present_and_family_matches() -> TestResult {
        let home = open_home()?;
        let mut service = home.service()?;
        let mission_id = MissionId::new("mission-resume")?;
        let admitted = service.admit_mission(
            mission_id.clone(),
            &valid_proposal(),
            &AuthoredParseContext::default(),
            "2026-07-24T00:00:00Z",
        )?;
        let phase_id = admitted.plan().phases[0].id.clone();
        let claude = RuntimeFamily::parse("claude")?;
        let handle = SessionHandle::new(claude.clone(), "live-session")?;
        service.record_continuation_handle(&admitted, &phase_id, 1, &handle)?;

        // Same family + durable prior handle -> resume.
        match service.select_continuation(&admitted, &phase_id, 2, 1, &claude)? {
            ContinuationDecision::ResumeSameProvider { handle } => {
                assert_eq!(handle.runtime_family().as_str(), "claude");
                assert_eq!(handle.expose_session_id(), "live-session");
            }
            ContinuationDecision::FreshFromCapsule { .. } => {
                return Err("same-family resume should have been selected".into());
            }
        }

        // No durable handle for the prior attempt -> fresh.
        let fresh = service.select_continuation(&admitted, &phase_id, 5, 1, &claude)?;
        assert!(matches!(
            fresh,
            ContinuationDecision::FreshFromCapsule { .. }
        ));
        service.close()?;
        Ok(())
    }

    #[test]
    fn unchanged_strategy_fingerprint_is_detectable_across_retries() -> TestResult {
        let home = open_home()?;
        let mut service = home.service()?;
        let mission_id = MissionId::new("mission-fingerprint")?;
        let admitted = service.admit_mission(
            mission_id.clone(),
            &valid_proposal(),
            &AuthoredParseContext::default(),
            "2026-07-24T00:00:00Z",
        )?;
        let phase_id = admitted.plan().phases[0].id.clone();
        let outcome = AttemptOutcome::incomplete(
            MechanicalTermination::ProviderStreamEnded,
            None,
            PartialWork::new(None, AttemptEvidence::new())?,
            Duration::from_millis(1),
        );
        // Two attempts with the SAME strategy inputs (same assignment, same
        // revision) -- a cosmetic retry.
        service.record_phase_attempt(
            &admitted,
            &phase_id,
            1,
            1,
            &outcome,
            "2026-07-24T00:00:01Z",
        )?;
        service.record_phase_attempt(
            &admitted,
            &phase_id,
            2,
            1,
            &outcome,
            "2026-07-24T00:00:02Z",
        )?;
        let prior = service.phase_strategy_fingerprints(&mission_id, &phase_id)?;
        assert_eq!(prior.len(), 2);
        assert_eq!(
            prior[0], prior[1],
            "an unchanged strategy must collide across retries"
        );

        // select_retry rejects a candidate that repeats a prior strategy, admits
        // a materially different one, and asks for a replan when the ladder is
        // exhausted.
        assert!(matches!(
            select_retry(&prior, Some(&prior[0])),
            Err(ContinuationError::StrategyUnchanged)
        ));
        assert_eq!(
            select_retry(&prior, Some("a-different-fingerprint"))?,
            RetryPlan::Retry
        );
        assert_eq!(select_retry(&prior, None)?, RetryPlan::ReplanRequired);
        service.close()?;
        Ok(())
    }

    #[test]
    fn keyword_fallback_admits_offline() -> TestResult {
        let home = open_home()?;
        let mut service = home.service()?;
        let mission_id = MissionId::new("mission-fallback")?;
        let proposal = keyword_fallback_proposal("research the topic and write findings");
        let admitted = service.admit_mission(
            mission_id.clone(),
            &proposal,
            &AuthoredParseContext::default(),
            "2026-07-24T00:00:00Z",
        )?;
        assert!(!admitted.plan().phases.is_empty());
        assert_eq!(service.reasoning_counts(&mission_id)?.records, 1);
        service.close()?;
        Ok(())
    }
}
