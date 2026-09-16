use crate::{
    ResolvedRuntimeHome,
    runtime_store::{JournalCommit, JournalIntent, RuntimeStore, RuntimeStoreError},
    usage_multiplexer::{
        BoundUsageObservation, PendingDispatch, UsageMultiplexerPolicy, assemble_routing_input,
    },
};
use orchestrator_core::{
    ADAPTIVE_ROUTING_SCHEMA_V1, AdaptiveCandidateV1, AdaptivePolicyV1, AdaptiveRoutingError,
    CandidateIdV1, ContinuationDispositionV1, FixedAuthorityProvenanceV1, FixedRouteV1, MissionId,
    ModelResolutionInput, ModelTier, PhaseId, ProviderIdV1, RouteDeferralReasonV1, RouteOutcomeV1,
    RouteRequirementsV1, RouteTargetV1, RoutingAuthorityV1, RoutingDecisionModeV1, RoutingMap,
    RoutingReplayRecordV1, RuntimeIdV1, RuntimeResolutionInput, RuntimeSource, UsageAccountIdV1,
    UtcMillisV1, decide_route, resolve_effort_for_runtime, resolve_model,
    resolve_model_for_runtime, resolve_runtime,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
};
use thiserror::Error;

/// Read-only capability used to inject routing configuration bytes.
pub trait ReadCapability {
    /// Reads a path relative to the already-resolved runtime home.
    /// `Ok(None)` represents a missing file.
    fn read_relative(
        &self,
        home: &ResolvedRuntimeHome,
        relative: &Path,
    ) -> io::Result<Option<Vec<u8>>>;
}

#[derive(Debug, Error)]
pub enum RoutingConfigError {
    #[error("cannot read routing configuration: {0}")]
    Read(#[source] io::Error),
    #[error("routing configuration is not UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("routing configuration is malformed YAML: {0}")]
    Malformed(#[from] serde_saphyr::Error),
}

/// A verbose-only degraded-run warning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoutingWarning {
    pub message: String,
}

/// Loads `config.yaml` through an injected read-only capability.
pub fn load_routing_map(
    home: &ResolvedRuntimeHome,
    source: &dyn ReadCapability,
) -> Result<RoutingMap, RoutingConfigError> {
    let Some(bytes) = source
        .read_relative(home, Path::new("config.yaml"))
        .map_err(RoutingConfigError::Read)?
    else {
        return Ok(RoutingMap::default());
    };
    let text = std::str::from_utf8(&bytes)?;
    serde_saphyr::from_str(text).map_err(RoutingConfigError::Malformed)
}

/// Converts a loader failure into the same empty-map degraded run behavior as Go.
#[must_use]
pub fn routing_map_for_run(
    result: Result<RoutingMap, RoutingConfigError>,
    verbose: bool,
) -> (RoutingMap, Option<RoutingWarning>) {
    match result {
        Ok(map) => (map, None),
        Err(error) => (
            RoutingMap::default(),
            verbose.then(|| RoutingWarning {
                message: format!("warning: {error}"),
            }),
        ),
    }
}

// ---------------------------------------------------------------------------
// Adaptive-routing dispatch boundary (ADR-0002 §2, §9, §10, §11, §12 items 4–5)
// ---------------------------------------------------------------------------
//
// Everything below is the composition-root half of ADR-0002 §12: it resolves
// the existing fixed runtime/model precedence first, assembles the trusted
// usage evidence through `usage_multiplexer`, calls the pure
// `orchestrator_core::decide_route` kernel exactly once, and persists the
// resulting decision plus its replay record through the ADR-0001 journal.
//
// The kernel owns §5–§10 semantics. This layer owns only what the kernel
// deliberately refuses to own: reading precedence from the existing resolver,
// supplying the incumbent and existing-session runtime that §9 hysteresis and
// tie-preference need, applying `dispatch_outcome` (never `recommended_outcome`,
// so §10 shadow mode keeps dispatching the legacy route), carrying the derived
// §10 continuation disposition outward, and durably recording the evidence.

/// The ADR-0001 journal transition kind for a completed routing decision.
pub const ROUTING_DECISION_TRANSITION_KIND: &str = "routing.decision";

/// The ADR-0001 journal transition kind for a fail-closed routing audit event.
pub const ROUTING_FAIL_CLOSED_TRANSITION_KIND: &str = "routing.decision.fail_closed";

/// Payload schema version for both routing journal transition kinds.
pub const ROUTING_JOURNAL_PAYLOAD_VERSION: u32 = 1;

/// ADR-0001 persistence boundary for routing evidence.
///
/// Defined at the consumer so the decision boundary can be exercised without a
/// live runtime home; [`RuntimeStore`] is the production implementation and
/// forwards to [`RuntimeStore::append`] unchanged, keeping the checksum chain,
/// idempotent exact-retry, and transition-conflict guarantees.
pub trait RoutingDecisionJournal {
    /// Appends one routing transition, returning its journal commit.
    fn append_routing_decision(
        &mut self,
        intent: &JournalIntent,
    ) -> Result<JournalCommit, RuntimeStoreError>;
}

impl RoutingDecisionJournal for RuntimeStore {
    fn append_routing_decision(
        &mut self,
        intent: &JournalIntent,
    ) -> Result<JournalCommit, RuntimeStoreError> {
        self.append(intent)
    }
}

/// Failures raised by the dispatch decision boundary itself.
///
/// Policy-evaluation failures are *not* represented here: they are handled
/// inside the boundary by failing closed onto the resolved fixed route and
/// emitting an audit transition. These variants are failures of the boundary's
/// own preconditions, and none of them permits a dispatch.
#[derive(Debug, Error)]
pub enum RoutingDispatchError {
    /// A composed identifier or route failed adaptive-routing validation.
    #[error("routing authority composition failed")]
    Composition(#[source] AdaptiveRoutingError),
    /// The routing evidence could not be recorded, so no dispatch is authorized.
    #[error("routing decision could not be journaled")]
    Journal(#[source] RuntimeStoreError),
    /// The routing evidence could not be encoded into a journal payload.
    #[error("routing decision could not be encoded")]
    Encode(#[source] serde_json::Error),
}

/// The non-secret identity every composed route for one dispatch shares.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteIdentity {
    /// Stable candidate identity for the composed route.
    pub candidate_id: CandidateIdV1,
    /// Provider family owning the route's usage budget.
    pub provider: ProviderIdV1,
    /// Non-secret account identity whose quota governs the route.
    pub usage_account_id: UsageAccountIdV1,
}

/// Fixed runtime/model authority inputs already owned by the composition root.
///
/// This layer does not reimplement precedence: it delegates to the existing
/// [`resolve_runtime`] and [`resolve_model`] resolvers and only records which
/// rung supplied the value, so ADR-0002 §2 provenance stays truthful.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixedAuthorityInputs {
    /// Shared non-secret identity for the composed route.
    pub identity: RouteIdentity,
    /// Runtime precedence inputs (authored, flag, environment, configured, policy).
    pub runtime: RuntimeResolutionInput,
    /// Explicit `--model` override, when one was supplied.
    pub forced_model: Option<String>,
    /// Whether a configured non-adaptive mode pinned the provider.
    pub fixed_provider_mode: bool,
    /// Whether the configured mode is the unchanged legacy router.
    pub legacy_mode: bool,
    /// Tier the existing router classified for this phase.
    pub tier: ModelTier,
    /// Persona name, consulted only by the non-Codex effort scale.
    pub persona: String,
    /// Read-only `config.yaml` routing map.
    pub routing_map: RoutingMap,
}

/// Outcome of fixed-authority resolution: the §2 authority the kernel consumes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedRoutingAuthority {
    authority: RoutingAuthorityV1,
    runtime_source: RuntimeSource,
    unknown_runtime_fell_back_to_claude: bool,
}

impl ResolvedRoutingAuthority {
    /// Returns the §2 authority handed to `decide_route`.
    #[must_use]
    pub const fn authority(&self) -> &RoutingAuthorityV1 {
        &self.authority
    }

    /// Returns the precedence rung that supplied the runtime.
    #[must_use]
    pub const fn runtime_source(&self) -> RuntimeSource {
        self.runtime_source
    }

    /// Returns whether an unsupported runtime value was executed as Claude.
    #[must_use]
    pub const fn unknown_runtime_fell_back_to_claude(&self) -> bool {
        self.unknown_runtime_fell_back_to_claude
    }

    /// Returns the exact fixed route when a §2 authority composed one.
    #[must_use]
    pub const fn fixed_route(&self) -> Option<&RouteTargetV1> {
        match &self.authority.resolved_fixed_route {
            Some(fixed) => Some(&fixed.route),
            None => None,
        }
    }
}

/// Resolves ADR-0002 §2 fixed authority before any adaptive policy is consulted.
///
/// Authored runtime, an explicit runtime flag, an explicit runtime environment
/// value, and an explicit model override each compose a fixed route that the
/// kernel then honors exactly. Configured-tier, policy, and default runtimes are
/// *not* fixed authority: they only produce the legacy route, leaving adaptive
/// policy free to choose.
pub fn resolve_fixed_authority(
    inputs: &FixedAuthorityInputs,
) -> Result<ResolvedRoutingAuthority, RoutingDispatchError> {
    let resolution = resolve_runtime(inputs.runtime.clone());
    let effective = resolution.runtime.clone();
    let forced_model = inputs
        .forced_model
        .as_ref()
        .filter(|model| !model.is_empty())
        .cloned();
    let model = resolve_model(ModelResolutionInput {
        forced_model: forced_model.clone(),
        tier: inputs.tier.as_str().to_owned(),
        effective_runtime: effective.clone(),
        routing_map: inputs.routing_map.clone(),
        built_in_model: resolve_model_for_runtime(inputs.tier, &effective).to_owned(),
    });
    let effort = resolve_effort_for_runtime(inputs.tier, &inputs.persona, &effective);

    let compose = |error| RoutingDispatchError::Composition(error);
    let legacy_route = RouteTargetV1 {
        candidate_id: inputs.identity.candidate_id.clone(),
        provider: inputs.identity.provider.clone(),
        usage_account_id: inputs.identity.usage_account_id.clone(),
        runtime: RuntimeIdV1::new(effective).map_err(compose)?,
        model,
        effort: Some(effort.to_owned()),
    };

    let mut provenances = BTreeSet::new();
    match resolution.source {
        RuntimeSource::Authored => {
            provenances.insert(FixedAuthorityProvenanceV1::AuthoredRuntime);
        }
        RuntimeSource::Forced => {
            provenances.insert(FixedAuthorityProvenanceV1::RuntimeFlag);
        }
        RuntimeSource::Environment => {
            provenances.insert(FixedAuthorityProvenanceV1::EnvironmentRuntime);
        }
        // Configured-tier, policy, and empty-default runtimes are not explicit
        // authority; they leave the adaptive kernel free to select.
        RuntimeSource::ConfiguredTier | RuntimeSource::Policy | RuntimeSource::EmptyDefault => {}
    }
    if forced_model.is_some() {
        provenances.insert(FixedAuthorityProvenanceV1::ModelFlag);
    }
    if inputs.fixed_provider_mode {
        provenances.insert(FixedAuthorityProvenanceV1::FixedProviderMode);
    }
    if inputs.legacy_mode {
        provenances.insert(FixedAuthorityProvenanceV1::LegacyMode);
    }

    let resolved_fixed_route = (!provenances.is_empty()).then(|| FixedRouteV1 {
        composition_version: ADAPTIVE_ROUTING_SCHEMA_V1,
        provenances,
        route: legacy_route.clone(),
    });

    Ok(ResolvedRoutingAuthority {
        authority: RoutingAuthorityV1 {
            resolved_fixed_route,
            legacy_route,
        },
        runtime_source: resolution.source,
        unknown_runtime_fell_back_to_claude: resolution.unknown_fell_back_to_claude,
    })
}

/// Everything one worker dispatch must present to the decision boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchDecisionRequest {
    /// Fixed runtime/model authority, resolved before adaptive policy (§2).
    pub authority_inputs: FixedAuthorityInputs,
    /// Mission identity used for audit correlation and the journal transition.
    pub mission_id: MissionId,
    /// Phase identity used for audit correlation and the journal transition.
    pub phase_id: PhaseId,
    /// One-based attempt number.
    pub attempt: u32,
    /// Caller-supplied evaluation time; this layer owns no clock.
    pub evaluated_at_utc_ms: UtcMillisV1,
    /// Caller-supplied RFC3339 UTC commit time for the journal transition.
    pub committed_at_utc: String,
    /// Hard task requirements.
    pub requirements: RouteRequirementsV1,
    /// Configured adaptive candidate catalog.
    pub candidates: Vec<AdaptiveCandidateV1>,
    /// Current candidate identity; supplied so §9 hysteresis can hold it.
    pub incumbent_candidate_id: Option<CandidateIdV1>,
    /// Existing runtime family; supplied so §9 tie-preference and the §10
    /// continuation disposition can be derived.
    pub existing_session_runtime: Option<RuntimeIdV1>,
    /// Explicit adaptive policy, including its `enforce`/`shadow` mode (§10).
    pub policy: AdaptivePolicyV1,
    /// Trusted usage-snapshot admission policy.
    pub multiplexer_policy: UsageMultiplexerPolicy,
    /// Capability-bound provider observations collected by trusted composition.
    pub observations: Vec<BoundUsageObservation>,
    /// Last accepted observation time per provider/account pair, for cooldown.
    pub last_accepted_observed_at: BTreeMap<(ProviderIdV1, UsageAccountIdV1), UtcMillisV1>,
}

/// What was durably recorded for one pass through the decision boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoutingJournalRecord {
    /// Deterministic ADR-0001 transition identifier.
    pub transition_id: String,
    /// Transition kind actually appended.
    pub kind: &'static str,
    /// Journal sequence assigned by the store.
    pub journal_sequence: i64,
    /// Whether an exact retry returned the original receipt.
    pub duplicate: bool,
}

/// Why the boundary fell closed instead of applying a policy decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailClosedCause {
    /// Trusted usage multiplexing rejected its own preconditions.
    UsageMultiplexer,
    /// `decide_route` rejected the policy or input.
    PolicyEvaluation,
    /// The replay record could not be constructed or did not recompute.
    ReplayRecord,
}

/// Result of the one decision boundary every worker dispatch passes through.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoutingDispatchOutcome {
    /// An exact route is authorized for dispatch.
    Dispatch {
        /// Route to execute. In `shadow` mode this is the legacy route (§10).
        route: RouteTargetV1,
        /// Session disposition derived from runtime family only (§10).
        continuation: ContinuationDispositionV1,
        /// Mode the kernel actually applied; `None` on a fail-closed dispatch.
        decision_mode: Option<RoutingDecisionModeV1>,
        /// Set when policy evaluation failed and the fixed route was applied.
        fail_closed: Option<FailClosedCause>,
        /// Durable evidence appended for this decision.
        record: RoutingJournalRecord,
    },
    /// No route may be dispatched.
    NoDispatch {
        /// Typed kernel deferral; `None` when policy evaluation itself failed
        /// and no fixed route existed to fail closed onto.
        reason: Option<RouteDeferralReasonV1>,
        /// Set when policy evaluation failed rather than deferring cleanly.
        fail_closed: Option<FailClosedCause>,
        /// Durable evidence appended for this decision.
        record: RoutingJournalRecord,
    },
}

impl RoutingDispatchOutcome {
    /// Returns the authorized route, if any.
    #[must_use]
    pub const fn route(&self) -> Option<&RouteTargetV1> {
        match self {
            Self::Dispatch { route, .. } => Some(route),
            Self::NoDispatch { .. } => None,
        }
    }

    /// Returns the durable evidence appended for this decision.
    #[must_use]
    pub const fn record(&self) -> &RoutingJournalRecord {
        match self {
            Self::Dispatch { record, .. } | Self::NoDispatch { record, .. } => record,
        }
    }
}

/// Derives the ADR-0002 §10 continuation disposition from the dispatch route.
///
/// Used only on the fail-closed path, where no `RoutingDecisionV1` exists to
/// read `continuation` from; a successful decision carries the kernel's own
/// derivation, which is applied verbatim. The kernel's rule is private, so this
/// restates it rather than reusing it.
///
/// Session identity is never consulted, carried, or compared — only the
/// validated runtime family.
fn continuation_for(
    route: Option<&RouteTargetV1>,
    existing_session_runtime: Option<&RuntimeIdV1>,
) -> ContinuationDispositionV1 {
    let Some(route) = route else {
        return ContinuationDispositionV1::NoDispatch;
    };
    let Some(existing) = existing_session_runtime else {
        return ContinuationDispositionV1::NoSession;
    };
    if existing == &route.runtime {
        ContinuationDispositionV1::SameRuntimeMayResume
    } else {
        ContinuationDispositionV1::FreshSessionRequired
    }
}

/// One decision per mission/phase/attempt.
///
/// [`RuntimeStore::append`] returns the original receipt for an exact retry and
/// [`RuntimeStoreError::TransitionConflict`] when the same transition ID carries
/// different bytes. A second, differing decision for one attempt is therefore
/// refused rather than silently overwriting the evidence the dispatch ran under.
fn decision_transition_id(mission: &MissionId, phase: &PhaseId, attempt: u32) -> String {
    format!(
        "routing-decision-{}-{}-{attempt}",
        mission.as_str(),
        phase.as_str()
    )
}

fn fail_closed_transition_id(mission: &MissionId, phase: &PhaseId, attempt: u32) -> String {
    format!(
        "routing-fail-closed-{}-{}-{attempt}",
        mission.as_str(),
        phase.as_str()
    )
}

fn append_routing_transition(
    journal: &mut dyn RoutingDecisionJournal,
    transition_id: String,
    kind: &'static str,
    mission_id: &MissionId,
    committed_at_utc: &str,
    payload: Value,
) -> Result<RoutingJournalRecord, RoutingDispatchError> {
    // No compatibility projection is required: routing evidence is Rust-private
    // audit material under ADR-0001, and Go has no projector for these kinds.
    // The journal row itself carries the checksum-chained, idempotent record.
    let intent = JournalIntent::new(
        transition_id.clone(),
        Some(mission_id.clone()),
        kind,
        payload,
        committed_at_utc.to_owned(),
    )
    .map_err(RoutingDispatchError::Journal)?;
    let commit = journal
        .append_routing_decision(&intent)
        .map_err(RoutingDispatchError::Journal)?;
    Ok(RoutingJournalRecord {
        transition_id,
        kind,
        journal_sequence: commit.sequence(),
        duplicate: commit.duplicate(),
    })
}

fn fail_closed_payload(
    cause: FailClosedCause,
    detail: &str,
    fixed_route: Option<&RouteTargetV1>,
) -> Result<Value, RoutingDispatchError> {
    let cause = match cause {
        FailClosedCause::UsageMultiplexer => "usage_multiplexer",
        FailClosedCause::PolicyEvaluation => "policy_evaluation",
        FailClosedCause::ReplayRecord => "replay_record",
    };
    let fixed_route = match fixed_route {
        Some(route) => serde_json::to_value(route).map_err(RoutingDispatchError::Encode)?,
        None => Value::Null,
    };
    Ok(serde_json::json!({
        "payload_version": ROUTING_JOURNAL_PAYLOAD_VERSION,
        "cause": cause,
        "detail": detail,
        "applied_fixed_route": fixed_route,
    }))
}

fn fail_closed(
    journal: &mut dyn RoutingDecisionJournal,
    request: &DispatchDecisionRequest,
    authority: &ResolvedRoutingAuthority,
    cause: FailClosedCause,
    detail: &str,
) -> Result<RoutingDispatchOutcome, RoutingDispatchError> {
    let fixed_route = authority.fixed_route();
    let payload = fail_closed_payload(cause, detail, fixed_route)?;
    let record = append_routing_transition(
        journal,
        fail_closed_transition_id(&request.mission_id, &request.phase_id, request.attempt),
        ROUTING_FAIL_CLOSED_TRANSITION_KIND,
        &request.mission_id,
        &request.committed_at_utc,
        payload,
    )?;

    // Fail closed onto the fixed route when §2 authority composed one. Without
    // a fixed route there is nothing to fall back *to*: dispatching the legacy
    // route here would be exactly the silent fallback ADR-0002 §9 forbids, so
    // the boundary refuses to dispatch instead.
    match fixed_route {
        Some(route) => Ok(RoutingDispatchOutcome::Dispatch {
            continuation: continuation_for(Some(route), request.existing_session_runtime.as_ref()),
            route: route.clone(),
            decision_mode: None,
            fail_closed: Some(cause),
            record,
        }),
        None => Ok(RoutingDispatchOutcome::NoDispatch {
            reason: None,
            fail_closed: Some(cause),
            record,
        }),
    }
}

/// The single adaptive-routing decision boundary for worker dispatch.
///
/// Order is contractual: fixed authority (§2) is resolved first, trusted usage
/// evidence is assembled second, `decide_route` runs third and immediately
/// before dispatch, and the decision plus its replay record are journaled
/// before the caller may act on the result. A caller that receives
/// [`RoutingDispatchOutcome::Dispatch`] has a durable record; a caller that
/// receives an error has no authorized route at all.
pub fn decide_dispatch_route(
    request: &DispatchDecisionRequest,
    journal: &mut dyn RoutingDecisionJournal,
) -> Result<RoutingDispatchOutcome, RoutingDispatchError> {
    let authority = resolve_fixed_authority(&request.authority_inputs)?;

    let pending = PendingDispatch {
        mission_id: request.mission_id.clone(),
        phase_id: request.phase_id.clone(),
        attempt: request.attempt,
        evaluated_at_utc_ms: request.evaluated_at_utc_ms,
        authority: authority.authority().clone(),
        requirements: request.requirements.clone(),
        candidates: request.candidates.clone(),
        incumbent_candidate_id: request.incumbent_candidate_id.clone(),
        existing_session_runtime: request.existing_session_runtime.clone(),
    };
    let input = match assemble_routing_input(
        pending,
        &request.multiplexer_policy,
        &request.observations,
        &request.last_accepted_observed_at,
    ) {
        Ok(input) => input,
        Err(error) => {
            return fail_closed(
                journal,
                request,
                &authority,
                FailClosedCause::UsageMultiplexer,
                &error.to_string(),
            );
        }
    };

    let decision = match decide_route(&request.policy, &input) {
        Ok(decision) => decision,
        Err(error) => {
            return fail_closed(
                journal,
                request,
                &authority,
                FailClosedCause::PolicyEvaluation,
                &error.to_string(),
            );
        }
    };

    // `RoutingReplayRecordV1::new` recomputes through `decide_route` and
    // requires structural equality, so the kernel runs twice per dispatch. That
    // is the contract's point — evidence that never round-tripped is not
    // replayable — and it is paid once per attempt, not per candidate.
    let replay = match RoutingReplayRecordV1::new(request.policy.clone(), input, decision.clone()) {
        Ok(replay) => replay,
        Err(error) => {
            return fail_closed(
                journal,
                request,
                &authority,
                FailClosedCause::ReplayRecord,
                &error.to_string(),
            );
        }
    };

    // The decision is stored beside the replay envelope even though the
    // envelope embeds it: `RoutingReplayRecordV1`'s fields are private and its
    // deserializer re-runs `decide_route` before yielding a record, so reading
    // the replay envelope costs a full policy evaluation. An audit consumer
    // that only wants to know what was dispatched reads `decision` directly.
    let payload = serde_json::json!({
        "payload_version": ROUTING_JOURNAL_PAYLOAD_VERSION,
        "decision": serde_json::to_value(&decision).map_err(RoutingDispatchError::Encode)?,
        "replay": serde_json::to_value(&replay).map_err(RoutingDispatchError::Encode)?,
    });
    let record = append_routing_transition(
        journal,
        decision_transition_id(&request.mission_id, &request.phase_id, request.attempt),
        ROUTING_DECISION_TRANSITION_KIND,
        &request.mission_id,
        &request.committed_at_utc,
        payload,
    )?;

    // §10: dispatch follows `dispatch_outcome`, never `recommended_outcome`, so
    // a shadow-mode adaptive recommendation is recorded without changing what
    // actually runs. §10 continuation is likewise derived from the dispatch
    // route by the kernel; it is carried out verbatim.
    Ok(match &decision.dispatch_outcome {
        RouteOutcomeV1::Route { route } => RoutingDispatchOutcome::Dispatch {
            route: route.clone(),
            continuation: decision.continuation,
            decision_mode: Some(decision.decision_mode),
            fail_closed: None,
            record,
        },
        RouteOutcomeV1::Deferred { reason } => RoutingDispatchOutcome::NoDispatch {
            reason: Some(*reason),
            fail_closed: None,
            record,
        },
    })
}

#[cfg(test)]
pub(crate) mod dispatch_tests {
    use super::*;
    use crate::usage_multiplexer::{ClaudeStatuslineAdapterPolicyV1, UsageMultiplexer};
    use orchestrator_core::{
        AdaptiveEvaluationModeV1, AutomaticEligibilityV1, BasisPointsV1, DurationMillisV1,
        ProviderReserveV1, QualityTierV1, RoutingDecisionV1, ScoreWeightsV1, TaskPriorityV1,
        UnknownUsagePolicyV1, replay_route,
    };
    use orchestrator_provider_claude::decode_claude_statusline_usage;

    type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

    /// Fixture journal: records every appended intent and can be made to fail
    /// so the boundary's "no durable record, no dispatch" rule is provable.
    #[derive(Default)]
    pub(crate) struct RecordingJournal {
        pub(crate) appended: Vec<(String, String, Value)>,
        pub(crate) reject: bool,
        next_sequence: i64,
    }

    impl RoutingDecisionJournal for RecordingJournal {
        fn append_routing_decision(
            &mut self,
            intent: &JournalIntent,
        ) -> Result<JournalCommit, RuntimeStoreError> {
            if self.reject {
                return Err(RuntimeStoreError::InvalidIntent(
                    "fixture rejects the intent",
                ));
            }
            self.appended.push((
                intent.transition_id().to_owned(),
                intent.kind().to_owned(),
                intent.payload().clone(),
            ));
            self.next_sequence += 1;
            Ok(JournalCommit::for_fixture(self.next_sequence))
        }
    }

    pub(crate) fn identity(account: &str) -> Fallible<RouteIdentity> {
        Ok(RouteIdentity {
            candidate_id: CandidateIdV1::new("claude-primary")?,
            provider: ProviderIdV1::new("claude")?,
            usage_account_id: UsageAccountIdV1::new(account)?,
        })
    }

    pub(crate) fn authority_inputs() -> Fallible<FixedAuthorityInputs> {
        Ok(FixedAuthorityInputs {
            identity: identity("acct-primary")?,
            runtime: RuntimeResolutionInput::default(),
            forced_model: None,
            fixed_provider_mode: false,
            legacy_mode: false,
            tier: ModelTier::Work,
            persona: "senior-backend-engineer".to_owned(),
            routing_map: RoutingMap::default(),
        })
    }

    fn candidate(
        id: &str,
        model: &str,
        runtime: &str,
        task_fit: u16,
    ) -> Fallible<AdaptiveCandidateV1> {
        Ok(AdaptiveCandidateV1 {
            route: RouteTargetV1 {
                candidate_id: CandidateIdV1::new(id)?,
                provider: ProviderIdV1::new("claude")?,
                usage_account_id: UsageAccountIdV1::new("acct-primary")?,
                runtime: RuntimeIdV1::new(runtime)?,
                model: model.to_owned(),
                effort: Some("medium".to_owned()),
            },
            capabilities: BTreeSet::new(),
            automatic_eligibility: AutomaticEligibilityV1::Automatic,
            quality: QualityTierV1::Standard,
            task_fit_bps: BasisPointsV1::new(task_fit)?,
            latency_bps: BasisPointsV1::new(10_000)?,
            configured_preference_bps: BasisPointsV1::new(10_000)?,
        })
    }

    pub(crate) fn policy(
        mode: AdaptiveEvaluationModeV1,
        switch_margin_bps: u16,
    ) -> Fallible<AdaptivePolicyV1> {
        Ok(AdaptivePolicyV1 {
            policy_version: 1,
            maximum_snapshot_age_ms: DurationMillisV1::new(300_000)?,
            provider_reserves: vec![ProviderReserveV1 {
                provider: ProviderIdV1::new("claude")?,
                usage_account_id: UsageAccountIdV1::new("acct-primary")?,
                reserve_bps: BasisPointsV1::new(0)?,
            }],
            score_weights: ScoreWeightsV1 {
                task_fit_bps: BasisPointsV1::new(4_000)?,
                usable_headroom_bps: BasisPointsV1::new(2_000)?,
                reset_proximity_bps: BasisPointsV1::new(1_000)?,
                recent_capacity_bps: BasisPointsV1::new(1_000)?,
                latency_bps: BasisPointsV1::new(1_000)?,
                health_bps: BasisPointsV1::new(500)?,
                configured_preference_bps: BasisPointsV1::new(500)?,
            },
            switch_margin_bps: BasisPointsV1::new(switch_margin_bps)?,
            unknown_usage_policy: UnknownUsagePolicyV1::Defer,
            evaluation_mode: mode,
        })
    }

    /// Observed just before the fixture's five-hour reset so the decoded windows
    /// satisfy the ADR-0002 §6 reset-horizon invariant.
    pub(crate) const OBSERVED_AT_MS: u64 = 1_738_425_600_000 - 60_000;

    pub(crate) fn observation() -> Fallible<BoundUsageObservation> {
        let bytes = br#"{
          "version":"2.1.211",
          "rate_limits":{
            "five_hour":{"used_percentage":10,"resets_at":1738425600},
            "seven_day":{"used_percentage":20,"resets_at":1738857600}
          }
        }"#;
        let mut owner = UsageMultiplexer::new();
        let handle = owner.enroll_claude_statusline(
            ProviderIdV1::new("claude")?,
            UsageAccountIdV1::new("acct-primary")?,
            ClaudeStatuslineAdapterPolicyV1::new(
                DurationMillisV1::new(120_000)?,
                BasisPointsV1::new(10_000)?,
                BasisPointsV1::new(9_000)?,
            )?,
        )?;
        Ok(owner.observe_claude_statusline(
            &handle,
            &decode_claude_statusline_usage(bytes)?,
            UtcMillisV1::new(OBSERVED_AT_MS),
            UtcMillisV1::new(OBSERVED_AT_MS),
        )?)
    }

    pub(crate) fn multiplexer_policy() -> Fallible<UsageMultiplexerPolicy> {
        Ok(UsageMultiplexerPolicy {
            maximum_snapshot_age_ms: DurationMillisV1::new(300_000)?,
            cooldown_ms: DurationMillisV1::new(60_000)?,
        })
    }

    pub(crate) fn request() -> Fallible<DispatchDecisionRequest> {
        Ok(DispatchDecisionRequest {
            authority_inputs: authority_inputs()?,
            mission_id: MissionId::new("20260826-dispatch")?,
            phase_id: PhaseId::new("phase-2")?,
            attempt: 1,
            evaluated_at_utc_ms: UtcMillisV1::new(OBSERVED_AT_MS + 1_000),
            committed_at_utc: "2026-08-26T04:53:13Z".to_owned(),
            requirements: RouteRequirementsV1 {
                priority: TaskPriorityV1::P1,
                required_capabilities: BTreeSet::new(),
                minimum_quality: QualityTierV1::Economy,
            },
            candidates: vec![candidate("claude-primary", "sonnet", "claude", 10_000)?],
            incumbent_candidate_id: None,
            existing_session_runtime: None,
            policy: policy(AdaptiveEvaluationModeV1::Enforce, 0)?,
            multiplexer_policy: multiplexer_policy()?,
            observations: vec![observation()?],
            last_accepted_observed_at: BTreeMap::new(),
        })
    }

    // --- §2: fixed authority is resolved before adaptive policy --------------

    #[test]
    fn authored_runtime_and_model_flag_both_compose_one_fixed_route() -> Fallible<()> {
        let mut inputs = authority_inputs()?;
        inputs.runtime.authored_runtime = Some("codex".to_owned());
        inputs.forced_model = Some("gpt-5.6-sol".to_owned());
        let resolved = resolve_fixed_authority(&inputs)?;

        let fixed = resolved
            .authority()
            .resolved_fixed_route
            .as_ref()
            .ok_or("expected a fixed route")?;
        // Runtime and model authority are orthogonal: both provenances survive
        // into one exact route rather than one discarding the other.
        assert_eq!(
            fixed.provenances,
            BTreeSet::from([
                FixedAuthorityProvenanceV1::AuthoredRuntime,
                FixedAuthorityProvenanceV1::ModelFlag,
            ])
        );
        assert_eq!(fixed.route.runtime.as_str(), "codex");
        assert_eq!(fixed.route.model, "gpt-5.6-sol");
        assert_eq!(resolved.runtime_source(), RuntimeSource::Authored);
        Ok(())
    }

    #[test]
    fn runtime_flag_and_environment_authority_are_fixed_but_configured_tier_is_not() -> Fallible<()>
    {
        let mut flagged = authority_inputs()?;
        flagged.runtime.runtime_policy_applied = true;
        flagged.runtime.forced_runtime = Some("codex".to_owned());
        assert_eq!(
            resolve_fixed_authority(&flagged)?
                .authority()
                .resolved_fixed_route
                .as_ref()
                .map(|fixed| fixed.provenances.clone()),
            Some(BTreeSet::from([FixedAuthorityProvenanceV1::RuntimeFlag]))
        );

        let mut environment = authority_inputs()?;
        environment.runtime.runtime_policy_applied = true;
        environment.runtime.environment_runtime = Some("codex".to_owned());
        assert_eq!(
            resolve_fixed_authority(&environment)?
                .authority()
                .resolved_fixed_route
                .as_ref()
                .map(|fixed| fixed.provenances.clone()),
            Some(BTreeSet::from([
                FixedAuthorityProvenanceV1::EnvironmentRuntime
            ]))
        );

        // A configured-tier runtime is a default, not explicit authority: it
        // must leave adaptive policy free to select.
        let mut configured = authority_inputs()?;
        configured.runtime.runtime_policy_applied = true;
        configured.runtime.configured_tier_runtime = Some("codex".to_owned());
        let resolved = resolve_fixed_authority(&configured)?;
        assert!(resolved.authority().resolved_fixed_route.is_none());
        assert_eq!(resolved.authority().legacy_route.runtime.as_str(), "codex");
        Ok(())
    }

    #[test]
    fn an_unsupported_authored_runtime_stays_authored_provenance_but_executes_as_claude()
    -> Fallible<()> {
        // Go carries the authored value verbatim into a case-sensitive executor
        // registry, so `CLAUDE` is unregistered and executes as Claude. The
        // route must reflect what actually runs while the provenance still
        // records that an author, not a default, chose it.
        let mut inputs = authority_inputs()?;
        inputs.runtime.authored_runtime = Some("CLAUDE".to_owned());
        let resolved = resolve_fixed_authority(&inputs)?;

        assert!(resolved.unknown_runtime_fell_back_to_claude());
        let fixed = resolved
            .authority()
            .resolved_fixed_route
            .as_ref()
            .ok_or("expected a fixed route")?;
        assert_eq!(
            fixed.provenances,
            BTreeSet::from([FixedAuthorityProvenanceV1::AuthoredRuntime])
        );
        assert_eq!(fixed.route.runtime.as_str(), "claude");
        Ok(())
    }

    #[test]
    fn one_attempt_owns_one_transition_id_and_a_conflict_authorizes_no_dispatch() -> Fallible<()> {
        let request = request()?;
        let mut journal = ConflictingJournal::default();
        let first = decide_dispatch_route(&request, &mut journal)?;
        assert_eq!(
            first.record().transition_id,
            format!(
                "routing-decision-{}-{}-1",
                request.mission_id.as_str(),
                request.phase_id.as_str()
            )
        );

        // The store's transition-conflict rule reaches the caller as an error,
        // never as an unrecorded dispatch.
        assert!(matches!(
            decide_dispatch_route(&request, &mut journal),
            Err(RoutingDispatchError::Journal(
                RuntimeStoreError::TransitionConflict
            ))
        ));
        Ok(())
    }

    /// Fixture standing in for [`RuntimeStore::append`]'s transition-conflict
    /// rule: the same transition ID may not be re-appended.
    #[derive(Default)]
    struct ConflictingJournal {
        seen: BTreeSet<String>,
    }

    impl RoutingDecisionJournal for ConflictingJournal {
        fn append_routing_decision(
            &mut self,
            intent: &JournalIntent,
        ) -> Result<JournalCommit, RuntimeStoreError> {
            if !self.seen.insert(intent.transition_id().to_owned()) {
                return Err(RuntimeStoreError::TransitionConflict);
            }
            Ok(JournalCommit::for_fixture(1))
        }
    }

    #[test]
    fn fixed_route_wins_even_when_it_is_absent_from_the_candidate_catalog() -> Fallible<()> {
        let mut request = request()?;
        request.authority_inputs.runtime.authored_runtime = Some("codex".to_owned());
        request.authority_inputs.forced_model = Some("gpt-5.6-sol".to_owned());
        let mut journal = RecordingJournal::default();

        let outcome = decide_dispatch_route(&request, &mut journal)?;
        let route = outcome.route().ok_or("expected a dispatch")?;
        assert_eq!(route.model, "gpt-5.6-sol");
        assert_eq!(route.runtime.as_str(), "codex");
        assert!(matches!(
            outcome,
            RoutingDispatchOutcome::Dispatch {
                decision_mode: Some(RoutingDecisionModeV1::Fixed),
                fail_closed: None,
                ..
            }
        ));
        Ok(())
    }

    // --- §9: hysteresis and stable selection --------------------------------

    #[test]
    fn incumbent_is_held_below_the_switch_margin_and_replaced_at_it() -> Fallible<()> {
        let base = request()?;
        let candidates = vec![
            candidate("claude-a", "sonnet", "claude", 4_000)?,
            candidate("claude-b", "opus", "claude", 10_000)?,
        ];

        // A margin wider than the whole score range holds the incumbent.
        let mut held = base.clone();
        held.candidates = candidates.clone();
        held.incumbent_candidate_id = Some(CandidateIdV1::new("claude-a")?);
        held.policy = policy(AdaptiveEvaluationModeV1::Enforce, 10_000)?;
        let mut journal = RecordingJournal::default();
        let outcome = decide_dispatch_route(&held, &mut journal)?;
        assert_eq!(
            outcome.route().ok_or("expected a dispatch")?.candidate_id,
            CandidateIdV1::new("claude-a")?
        );

        // A zero margin is inclusive, so the higher-scoring challenger wins.
        let mut switched = base;
        switched.candidates = candidates;
        switched.incumbent_candidate_id = Some(CandidateIdV1::new("claude-a")?);
        switched.policy = policy(AdaptiveEvaluationModeV1::Enforce, 0)?;
        let mut journal = RecordingJournal::default();
        let outcome = decide_dispatch_route(&switched, &mut journal)?;
        assert_eq!(
            outcome.route().ok_or("expected a dispatch")?.candidate_id,
            CandidateIdV1::new("claude-b")?
        );
        Ok(())
    }

    // --- §10: shadow dispatch and session disposition -----------------------

    #[test]
    fn shadow_mode_records_the_adaptive_recommendation_but_dispatches_the_legacy_route()
    -> Fallible<()> {
        let mut request = request()?;
        // Legacy resolves to sonnet; the only adaptive candidate is opus, so a
        // shadow-mode dispatch that changed behavior would be visible.
        request.candidates = vec![candidate("claude-b", "opus", "claude", 10_000)?];
        request.policy = policy(AdaptiveEvaluationModeV1::Shadow, 0)?;
        let mut journal = RecordingJournal::default();

        let outcome = decide_dispatch_route(&request, &mut journal)?;
        let route = outcome.route().ok_or("expected a dispatch")?;
        assert_eq!(route.model, "sonnet");

        let (_, _, payload) = journal.appended.first().ok_or("expected a record")?;
        let decision: RoutingDecisionV1 = serde_json::from_value(payload["decision"].clone())?;
        assert_eq!(
            decision
                .recommended_outcome
                .route()
                .ok_or("expected a recommendation")?
                .model,
            "opus"
        );
        Ok(())
    }

    #[test]
    fn continuation_follows_the_dispatch_runtime_family_only() -> Fallible<()> {
        let mut same = request()?;
        same.existing_session_runtime = Some(RuntimeIdV1::new("claude")?);
        let mut journal = RecordingJournal::default();
        assert!(matches!(
            decide_dispatch_route(&same, &mut journal)?,
            RoutingDispatchOutcome::Dispatch {
                continuation: ContinuationDispositionV1::SameRuntimeMayResume,
                ..
            }
        ));

        let mut crossed = request()?;
        crossed.existing_session_runtime = Some(RuntimeIdV1::new("codex")?);
        let mut journal = RecordingJournal::default();
        assert!(matches!(
            decide_dispatch_route(&crossed, &mut journal)?,
            RoutingDispatchOutcome::Dispatch {
                continuation: ContinuationDispositionV1::FreshSessionRequired,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn a_kernel_deferral_yields_no_dispatch_and_still_records_evidence() -> Fallible<()> {
        let mut request = request()?;
        // No trustworthy usage evidence at all: `unknown_usage_policy: Defer`
        // must fail closed rather than dispatch anything.
        request.observations.clear();
        let mut journal = RecordingJournal::default();

        let outcome = decide_dispatch_route(&request, &mut journal)?;
        assert!(matches!(
            outcome,
            RoutingDispatchOutcome::NoDispatch {
                reason: Some(RouteDeferralReasonV1::UnknownUsage),
                fail_closed: None,
                ..
            }
        ));
        assert_eq!(journal.appended.len(), 1);
        assert_eq!(journal.appended[0].1, ROUTING_DECISION_TRANSITION_KIND);
        Ok(())
    }

    // --- §11: persistence and replay ----------------------------------------

    #[test]
    fn the_persisted_replay_record_recomputes_to_the_persisted_decision() -> Fallible<()> {
        let request = request()?;
        let mut journal = RecordingJournal::default();
        let outcome = decide_dispatch_route(&request, &mut journal)?;

        let (transition_id, kind, payload) = journal.appended.first().ok_or("expected a record")?;
        assert_eq!(kind, ROUTING_DECISION_TRANSITION_KIND);
        assert_eq!(transition_id, &outcome.record().transition_id);
        assert_eq!(payload["payload_version"], ROUTING_JOURNAL_PAYLOAD_VERSION);

        // The replay envelope must survive a full wire round trip and recompute
        // to exactly the decision that was recorded alongside it.
        let replay: RoutingReplayRecordV1 = serde_json::from_value(payload["replay"].clone())?;
        let recomputed = replay_route(&replay)?;
        let persisted: RoutingDecisionV1 = serde_json::from_value(payload["decision"].clone())?;
        assert_eq!(recomputed, persisted);
        assert_eq!(persisted.dispatch_outcome.route(), outcome.route());
        Ok(())
    }

    #[test]
    fn a_journal_failure_authorizes_no_dispatch() -> Fallible<()> {
        let request = request()?;
        let mut journal = RecordingJournal {
            reject: true,
            ..RecordingJournal::default()
        };
        assert!(matches!(
            decide_dispatch_route(&request, &mut journal),
            Err(RoutingDispatchError::Journal(_))
        ));
        Ok(())
    }

    // --- Fail-closed policy evaluation --------------------------------------

    fn unsupported_policy_version(request: &mut DispatchDecisionRequest) {
        // Version 2 is unsupported, so `decide_route` rejects rather than
        // guessing — the exact class of failure this boundary must absorb.
        request.policy.policy_version = 2;
    }

    #[test]
    fn policy_failure_falls_closed_to_the_fixed_route_with_an_audit_event() -> Fallible<()> {
        let mut request = request()?;
        request.authority_inputs.runtime.authored_runtime = Some("codex".to_owned());
        request.existing_session_runtime = Some(RuntimeIdV1::new("claude")?);
        // A universal request invariant, which §2 validates *before* the fixed
        // route wins. An unsupported policy version would not do: under fixed
        // authority the kernel never reads the adaptive policy at all.
        request.attempt = 0;
        let mut journal = RecordingJournal::default();

        let outcome = decide_dispatch_route(&request, &mut journal)?;
        let RoutingDispatchOutcome::Dispatch {
            route,
            continuation,
            decision_mode,
            fail_closed,
            ..
        } = &outcome
        else {
            return Err("expected a fail-closed dispatch".into());
        };
        assert_eq!(route.runtime.as_str(), "codex");
        assert_eq!(
            *continuation,
            ContinuationDispositionV1::FreshSessionRequired
        );
        assert!(decision_mode.is_none());
        assert_eq!(*fail_closed, Some(FailClosedCause::PolicyEvaluation));

        let (_, kind, payload) = journal.appended.first().ok_or("expected an audit event")?;
        assert_eq!(kind, ROUTING_FAIL_CLOSED_TRANSITION_KIND);
        assert_eq!(payload["cause"], "policy_evaluation");
        assert_eq!(payload["applied_fixed_route"]["runtime"], "codex");
        Ok(())
    }

    #[test]
    fn policy_failure_without_fixed_authority_never_falls_back_to_the_legacy_route() -> Fallible<()>
    {
        let mut request = request()?;
        unsupported_policy_version(&mut request);
        let mut journal = RecordingJournal::default();

        let outcome = decide_dispatch_route(&request, &mut journal)?;
        assert!(matches!(
            outcome,
            RoutingDispatchOutcome::NoDispatch {
                reason: None,
                fail_closed: Some(FailClosedCause::PolicyEvaluation),
                ..
            }
        ));
        let (_, kind, payload) = journal.appended.first().ok_or("expected an audit event")?;
        assert_eq!(kind, ROUTING_FAIL_CLOSED_TRANSITION_KIND);
        assert_eq!(payload["applied_fixed_route"], Value::Null);
        Ok(())
    }

    #[test]
    fn a_replay_record_failure_is_audited_as_its_own_cause() -> Fallible<()> {
        let mut request = request()?;
        request.authority_inputs.runtime.authored_runtime = Some("claude".to_owned());
        // Duplicate reserves are invalid policy, but §2 lets `decide_route`
        // ignore the adaptive policy entirely under fixed authority. The
        // contradiction only surfaces when the replay envelope is built, which
        // must still fail closed rather than dispatch un-replayable evidence.
        let reserve = ProviderReserveV1 {
            provider: ProviderIdV1::new("claude")?,
            usage_account_id: UsageAccountIdV1::new("acct-primary")?,
            reserve_bps: BasisPointsV1::new(0)?,
        };
        request.policy.provider_reserves = vec![reserve.clone(), reserve];
        let mut journal = RecordingJournal::default();

        let outcome = decide_dispatch_route(&request, &mut journal)?;
        assert!(matches!(
            outcome,
            RoutingDispatchOutcome::Dispatch {
                fail_closed: Some(FailClosedCause::ReplayRecord),
                ..
            }
        ));
        assert_eq!(journal.appended[0].1, ROUTING_FAIL_CLOSED_TRANSITION_KIND);
        assert_eq!(journal.appended[0].2["cause"], "replay_record");
        Ok(())
    }

    #[test]
    fn a_usage_multiplexer_failure_is_audited_as_its_own_cause() -> Fallible<()> {
        let mut request = request()?;
        request.authority_inputs.runtime.authored_runtime = Some("claude".to_owned());
        // Evaluating before local receipt is the multiplexer's own typed
        // precondition failure, distinct from a policy rejection.
        request.evaluated_at_utc_ms = UtcMillisV1::new(0);
        let mut journal = RecordingJournal::default();

        let outcome = decide_dispatch_route(&request, &mut journal)?;
        assert!(matches!(
            outcome,
            RoutingDispatchOutcome::Dispatch {
                fail_closed: Some(FailClosedCause::UsageMultiplexer),
                ..
            }
        ));
        assert_eq!(journal.appended[0].2["cause"], "usage_multiplexer");
        Ok(())
    }
}
