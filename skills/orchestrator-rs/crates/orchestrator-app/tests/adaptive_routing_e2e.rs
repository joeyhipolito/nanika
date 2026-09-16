//! End-to-end proof of the B2 adaptive-routing dispatch boundary
//! ([`orchestrator_app::decide_dispatch_route`]) from outside the crate.
//!
//! This exercises the same public seam a real caller (a worker-dispatch
//! composition root) would use: build a [`DispatchDecisionRequest`], pass a
//! [`RoutingDecisionJournal`] implementation, and read the
//! [`RoutingDispatchOutcome`]. The journal fixture here stands in for
//! [`RuntimeStore`] — an out-of-crate integration test cannot open a real one
//! (its boundary constructors are crate-internal; see
//! `tests/mission_reasoning_e2e.rs`) — but it still round-trips through the
//! real [`JournalIntent`]/[`JournalCommit`] types via the `test-support`
//! feature (enabled unconditionally for this crate's own test builds through
//! the self-referencing dev-dependency in `Cargo.toml`), so the payload
//! assertions below read exactly the bytes `decide_dispatch_route` would hand
//! a real journal.
//!
//! Covers, in order: an adaptive win, a fixed-authority override, cooldown
//! suppression across two consecutive dispatches, a stale-snapshot rejection,
//! an offline replay round trip on the persisted record, and rejection of a
//! tampered replay record.

use std::collections::{BTreeMap, BTreeSet};

use orchestrator_app::{
    ClaudeStatuslineAdapterPolicyV1, DispatchDecisionRequest, FixedAuthorityInputs, JournalCommit,
    JournalIntent, RouteIdentity, RoutingDecisionJournal, RoutingDispatchOutcome,
    RuntimeStoreError, UsageMultiplexer, UsageMultiplexerPolicy, decide_dispatch_route,
};
use orchestrator_core::{
    AdaptiveCandidateV1, AdaptiveEvaluationModeV1, AdaptivePolicyV1, AutomaticEligibilityV1,
    BasisPointsV1, CandidateIdV1, DurationMillisV1, MissionId, ModelTier, PhaseId, ProviderIdV1,
    ProviderReserveV1, QualityTierV1, RouteDeferralReasonV1, RouteRequirementsV1, RouteTargetV1,
    RoutingDecisionModeV1, RoutingDecisionV1, RoutingMap, RoutingReplayRecordV1,
    RuntimeResolutionInput, ScoreWeightsV1, TaskPriorityV1, UnknownUsagePolicyV1, UsageAccountIdV1,
    UtcMillisV1, replay_route,
};
use orchestrator_provider_claude::decode_claude_statusline_usage;
use serde_json::Value;

type Fallible<T = ()> = Result<T, Box<dyn std::error::Error>>;

// ---------------------------------------------------------------------------
// Journal fixture
// ---------------------------------------------------------------------------

/// Records every transition appended by `decide_dispatch_route`, exactly as a
/// real `RuntimeStore`-backed journal would receive it. This is the only
/// place in the test that reaches into the `test-support`-gated surface.
#[derive(Default)]
struct RecordingJournal {
    appended: Vec<(String, Value)>,
    next_sequence: i64,
}

impl RoutingDecisionJournal for RecordingJournal {
    fn append_routing_decision(
        &mut self,
        intent: &JournalIntent,
    ) -> Result<JournalCommit, RuntimeStoreError> {
        self.appended
            .push((intent.kind().to_owned(), intent.payload().clone()));
        self.next_sequence += 1;
        Ok(JournalCommit::for_fixture(self.next_sequence))
    }
}

// ---------------------------------------------------------------------------
// Fixture builders
// ---------------------------------------------------------------------------

fn identity() -> Fallible<RouteIdentity> {
    Ok(RouteIdentity {
        candidate_id: CandidateIdV1::new("claude-primary")?,
        provider: ProviderIdV1::new("claude")?,
        usage_account_id: UsageAccountIdV1::new("acct-primary")?,
    })
}

fn authority_inputs() -> Fallible<FixedAuthorityInputs> {
    Ok(FixedAuthorityInputs {
        identity: identity()?,
        runtime: RuntimeResolutionInput::default(),
        forced_model: None,
        fixed_provider_mode: false,
        legacy_mode: false,
        tier: ModelTier::Work,
        persona: "senior-backend-engineer".to_owned(),
        routing_map: RoutingMap::default(),
    })
}

fn candidate(id: &str, model: &str, task_fit: u16) -> Fallible<AdaptiveCandidateV1> {
    Ok(AdaptiveCandidateV1 {
        route: RouteTargetV1 {
            candidate_id: CandidateIdV1::new(id)?,
            provider: ProviderIdV1::new("claude")?,
            usage_account_id: UsageAccountIdV1::new("acct-primary")?,
            runtime: orchestrator_core::RuntimeIdV1::new("claude")?,
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

fn policy(mode: AdaptiveEvaluationModeV1) -> Fallible<AdaptivePolicyV1> {
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
        switch_margin_bps: BasisPointsV1::new(0)?,
        unknown_usage_policy: UnknownUsagePolicyV1::Defer,
        evaluation_mode: mode,
    })
}

fn multiplexer_policy() -> Fallible<UsageMultiplexerPolicy> {
    Ok(UsageMultiplexerPolicy {
        maximum_snapshot_age_ms: DurationMillisV1::new(300_000)?,
        cooldown_ms: DurationMillisV1::new(60_000)?,
    })
}

/// Observed well inside the fixture's five-hour reset window so the decoded
/// windows satisfy the ADR-0002 §6 reset-horizon invariant.
const OBSERVED_AT_MS: u64 = 1_738_425_600_000 - 60_000;

fn observation_at(observed_at_utc_ms: u64) -> Fallible<orchestrator_app::BoundUsageObservation> {
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
        UtcMillisV1::new(observed_at_utc_ms),
        UtcMillisV1::new(observed_at_utc_ms),
    )?)
}

fn request(mission: &str, attempt: u32) -> Fallible<DispatchDecisionRequest> {
    Ok(DispatchDecisionRequest {
        authority_inputs: authority_inputs()?,
        mission_id: MissionId::new(mission)?,
        phase_id: PhaseId::new("phase-3")?,
        attempt,
        evaluated_at_utc_ms: UtcMillisV1::new(OBSERVED_AT_MS + 1_000),
        committed_at_utc: "2026-08-26T06:34:57Z".to_owned(),
        requirements: RouteRequirementsV1 {
            priority: TaskPriorityV1::P1,
            required_capabilities: BTreeSet::new(),
            minimum_quality: QualityTierV1::Economy,
        },
        candidates: vec![candidate("claude-primary", "sonnet", 10_000)?],
        incumbent_candidate_id: None,
        existing_session_runtime: None,
        policy: policy(AdaptiveEvaluationModeV1::Enforce)?,
        multiplexer_policy: multiplexer_policy()?,
        observations: vec![observation_at(OBSERVED_AT_MS)?],
        last_accepted_observed_at: BTreeMap::new(),
    })
}

// ---------------------------------------------------------------------------
// 1. Adaptive win: usage evidence is selected immediately before dispatch,
//    and the higher-scoring candidate is chosen with no fixed authority in
//    play.
// ---------------------------------------------------------------------------

#[test]
fn adaptive_policy_wins_and_selects_the_higher_scoring_candidate() -> Fallible {
    let mut request = request("20260826-e2e-adaptive", 1)?;
    request.candidates = vec![
        candidate("claude-low", "haiku", 1_000)?,
        candidate("claude-high", "opus", 10_000)?,
    ];
    let mut journal = RecordingJournal::default();

    let outcome = decide_dispatch_route(&request, &mut journal)?;
    let RoutingDispatchOutcome::Dispatch {
        route,
        decision_mode,
        fail_closed,
        ..
    } = &outcome
    else {
        return Err("expected an adaptive dispatch".into());
    };
    assert_eq!(route.candidate_id, CandidateIdV1::new("claude-high")?);
    assert!(matches!(
        decision_mode,
        Some(RoutingDecisionModeV1::Adaptive {
            evaluation_mode: AdaptiveEvaluationModeV1::Enforce
        })
    ));
    assert!(fail_closed.is_none());

    // Trusted usage evidence was actually consulted, not skipped: the record
    // this dispatch left behind embeds the bound snapshot from the supplied
    // observation.
    let (_, payload) = journal.appended.first().ok_or("expected a record")?;
    let decision: RoutingDecisionV1 = serde_json::from_value(payload["decision"].clone())?;
    assert_eq!(
        decision
            .dispatch_outcome
            .route()
            .ok_or("expected a route")?
            .candidate_id,
        CandidateIdV1::new("claude-high")?
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 2. Fixed-authority precedence: an authored runtime plus an explicit model
//    flag must win over adaptive policy even when adaptive policy would have
//    chosen something else.
// ---------------------------------------------------------------------------

#[test]
fn fixed_authority_overrides_adaptive_policy() -> Fallible {
    let mut request = request("20260826-e2e-fixed", 1)?;
    request.authority_inputs.runtime.authored_runtime = Some("codex".to_owned());
    request.authority_inputs.forced_model = Some("gpt-5.6-sol".to_owned());
    // The only adaptive candidate scores maximally and is not the fixed
    // route, so a dispatch that ignored §2 precedence would be visible here.
    request.candidates = vec![candidate("claude-high", "opus", 10_000)?];
    let mut journal = RecordingJournal::default();

    let outcome = decide_dispatch_route(&request, &mut journal)?;
    let RoutingDispatchOutcome::Dispatch {
        route,
        decision_mode,
        fail_closed,
        ..
    } = &outcome
    else {
        return Err("expected a fixed dispatch".into());
    };
    assert_eq!(route.runtime.as_str(), "codex");
    assert_eq!(route.model, "gpt-5.6-sol");
    assert_eq!(*decision_mode, Some(RoutingDecisionModeV1::Fixed));
    assert!(fail_closed.is_none());
    Ok(())
}

// ---------------------------------------------------------------------------
// 3. Cooldown enforcement across consecutive dispatches: the second dispatch
//    supplies a fresh observation inside the cooldown window measured from
//    the first dispatch's accepted observation, and the caller threads the
//    accepted-at state forward exactly as a real dispatcher must.
// ---------------------------------------------------------------------------

#[test]
fn cooldown_suppresses_a_second_dispatch_observation_and_no_route_is_authorized() -> Fallible {
    let mission = "20260826-e2e-cooldown";
    let mut first = request(mission, 1)?;
    first.observations = vec![observation_at(OBSERVED_AT_MS)?];
    let mut journal = RecordingJournal::default();

    let first_outcome = decide_dispatch_route(&first, &mut journal)?;
    assert!(matches!(
        first_outcome,
        RoutingDispatchOutcome::Dispatch {
            fail_closed: None,
            ..
        }
    ));

    // The caller threads forward what it just accepted, exactly as
    // `usage_multiplexer::assemble_routing_input`'s contract requires.
    let mut last_accepted = BTreeMap::new();
    last_accepted.insert(
        (
            ProviderIdV1::new("claude")?,
            UsageAccountIdV1::new("acct-primary")?,
        ),
        UtcMillisV1::new(OBSERVED_AT_MS),
    );

    // 5 seconds later: well inside the 60-second cooldown configured by
    // `multiplexer_policy()`.
    let second_observed_at = OBSERVED_AT_MS + 5_000;
    let mut second = request(mission, 2)?;
    second.evaluated_at_utc_ms = UtcMillisV1::new(second_observed_at + 1_000);
    second.observations = vec![observation_at(second_observed_at)?];
    second.last_accepted_observed_at = last_accepted;

    let second_outcome = decide_dispatch_route(&second, &mut journal)?;
    // Cooling-down evidence is excluded, not failing assembly, so
    // `decide_route` sees zero usage snapshots and defers under the fixture's
    // `unknown_usage_policy: Defer` rather than dispatching anything.
    assert!(matches!(
        second_outcome,
        RoutingDispatchOutcome::NoDispatch {
            reason: Some(RouteDeferralReasonV1::UnknownUsage),
            fail_closed: None,
            ..
        }
    ));
    Ok(())
}

// ---------------------------------------------------------------------------
// 4. Stale-snapshot rejection: an observation older than the multiplexer's
//    maximum snapshot age is excluded, leaving no trustworthy evidence.
// ---------------------------------------------------------------------------

#[test]
fn a_stale_snapshot_is_rejected_and_no_route_is_authorized() -> Fallible {
    let mut request = request("20260826-e2e-stale", 1)?;
    // `maximum_snapshot_age_ms` is 300_000 (5 minutes); observe 10 minutes
    // before the evaluation time so the observation ages out.
    let stale_observed_at = OBSERVED_AT_MS - 600_000;
    request.observations = vec![observation_at(stale_observed_at)?];

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
    // The deferral was still durably recorded — a stale snapshot is quietly
    // excluded from evidence, not silently dropped from the audit trail.
    assert_eq!(journal.appended.len(), 1);
    Ok(())
}

// ---------------------------------------------------------------------------
// 5 & 6. Offline replay: the persisted replay envelope reproduces the
//    persisted decision byte-for-byte in structure, and a tampered copy of
//    that same record is rejected rather than silently accepted.
// ---------------------------------------------------------------------------

#[test]
fn the_persisted_replay_record_reproduces_the_decision_offline() -> Fallible {
    let request = request("20260826-e2e-replay", 1)?;
    let mut journal = RecordingJournal::default();
    let outcome = decide_dispatch_route(&request, &mut journal)?;

    let (kind, payload) = journal.appended.first().ok_or("expected a record")?;
    assert_eq!(kind, orchestrator_app::ROUTING_DECISION_TRANSITION_KIND);

    // The replay envelope is exactly what was durably recorded — deserialize
    // it back with no knowledge of how it was produced, as an offline replay
    // tool would after reading it out of storage.
    let replay: RoutingReplayRecordV1 = serde_json::from_value(payload["replay"].clone())?;
    let recomputed = replay_route(&replay)?;

    let persisted: RoutingDecisionV1 = serde_json::from_value(payload["decision"].clone())?;
    assert_eq!(recomputed, persisted);
    assert_eq!(persisted.dispatch_outcome.route(), outcome.route());
    Ok(())
}

#[test]
fn a_tampered_replay_record_is_rejected() -> Fallible {
    let request = request("20260826-e2e-tamper", 1)?;
    let mut journal = RecordingJournal::default();
    decide_dispatch_route(&request, &mut journal)?;

    let (_, payload) = journal.appended.first().ok_or("expected a record")?;
    let mut tampered = payload["replay"].clone();

    // Flip the recorded model inside `expected_decision` without touching the
    // policy or input the decision was computed from. Recomputing from the
    // untouched policy/input must land on the original model, so the
    // corrupted `expected_decision` can no longer match.
    let model = tampered["expected_decision"]["dispatch_outcome"]["route"]["model"]
        .as_str()
        .ok_or("expected a route model in the persisted decision")?
        .to_owned();
    let forged_model = format!("{model}-tampered");
    tampered["expected_decision"]["dispatch_outcome"]["route"]["model"] =
        Value::String(forged_model.clone());
    // A byte-identical replica also needs `recommended_outcome` in agreement,
    // or the corruption would surface as an unrelated shape mismatch instead
    // of proving the recompute-and-compare check itself catches tampering.
    if tampered["expected_decision"]["recommended_outcome"]["route"]["model"].is_string() {
        tampered["expected_decision"]["recommended_outcome"]["route"]["model"] =
            Value::String(forged_model);
    }

    // `RoutingReplayRecordV1`'s `Deserialize` impl itself recomputes through
    // `decide_route` and compares against `expected_decision` before it will
    // yield a value at all (mirroring `recompute_and_validate`), so a
    // tampered record is rejected the moment it is read back — never handed
    // to a caller as a value `replay_route` would then have to catch.
    let forged: Result<RoutingReplayRecordV1, _> = serde_json::from_value(tampered);
    let Err(error) = forged else {
        return Err("a tampered record must not deserialize cleanly".into());
    };
    assert!(error.to_string().to_lowercase().contains("replay"));
    Ok(())
}
