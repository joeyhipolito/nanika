use orchestrator_core::{
    ADAPTIVE_MAX_DURATION_MS_V1, ADAPTIVE_POLICY_VERSION_V1, ADAPTIVE_ROUTING_SCHEMA_V1,
    AdaptiveCandidateV1, AdaptiveEvaluationModeV1, AdaptivePolicyV1, AdaptiveRoutingError,
    AppliedAuthorityV1, AutomaticEligibilityV1, BasisPointsV1, CandidateDispositionV1,
    CandidateEvaluationV1, CandidateIdV1, CandidateRejectionV1, CapabilityIdV1,
    ContinuationDispositionV1, DurationMillisV1, FixedAuthorityProvenanceV1, FixedRouteV1,
    MissionId, PhaseId, ProviderIdV1, ProviderReserveV1, QualityTierV1, RouteDeferralReasonV1,
    RouteOutcomeV1, RouteRequirementsV1, RouteTargetV1, RoutingAuthorityV1, RoutingDecisionModeV1,
    RoutingDecisionV1, RoutingInputV1, RoutingReplayRecordV1, RuntimeIdV1, ScoreWeightsV1,
    SnapshotIdV1, SwitchReasonV1, TaskPriorityV1, UnknownUsagePolicyV1, UsageAccountIdV1,
    UsageHealthV1, UsageLimitIdV1, UsageReasonCodeV1, UsageSnapshotV1, UsageSourceIdV1,
    UsageWindowKindV1, UsageWindowV1, UtcMillisV1, decide_route, replay_route,
};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeSet, error::Error};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn bp(value: u16) -> Result<BasisPointsV1, AdaptiveRoutingError> {
    BasisPointsV1::new(value)
}

fn route(
    candidate_id: &str,
    provider: &str,
    runtime: &str,
) -> Result<RouteTargetV1, AdaptiveRoutingError> {
    Ok(RouteTargetV1 {
        candidate_id: CandidateIdV1::new(candidate_id)?,
        provider: ProviderIdV1::new(provider)?,
        usage_account_id: UsageAccountIdV1::new("primary")?,
        runtime: RuntimeIdV1::new(runtime)?,
        model: format!("{candidate_id}-model"),
        effort: Some("high".to_owned()),
    })
}

fn candidate(
    candidate_id: &str,
    provider: &str,
    runtime: &str,
    task_fit: u16,
) -> Result<AdaptiveCandidateV1, AdaptiveRoutingError> {
    Ok(AdaptiveCandidateV1 {
        route: route(candidate_id, provider, runtime)?,
        capabilities: BTreeSet::from([CapabilityIdV1::new("tools")?]),
        automatic_eligibility: AutomaticEligibilityV1::Automatic,
        quality: QualityTierV1::Premium,
        task_fit_bps: bp(task_fit)?,
        latency_bps: bp(5_000)?,
        configured_preference_bps: bp(5_000)?,
    })
}

fn snapshot(
    snapshot_id: &str,
    provider: &str,
    remaining: u16,
    received_at: u64,
    ttl_ms: u64,
    reset_at: u64,
) -> Result<UsageSnapshotV1, AdaptiveRoutingError> {
    Ok(UsageSnapshotV1 {
        schema_version: ADAPTIVE_ROUTING_SCHEMA_V1,
        snapshot_id: SnapshotIdV1::new(snapshot_id)?,
        provider: ProviderIdV1::new(provider)?,
        usage_account_id: UsageAccountIdV1::new("primary")?,
        source: UsageSourceIdV1::new("fixture")?,
        observed_at_utc_ms: UtcMillisV1::new(received_at),
        received_at_utc_ms: UtcMillisV1::new(received_at),
        ttl_ms: DurationMillisV1::new(ttl_ms)?,
        confidence_bps: bp(10_000)?,
        health: UsageHealthV1::Healthy,
        reason_code: None,
        windows: vec![UsageWindowV1 {
            limit_id: UsageLimitIdV1::new("primary")?,
            kind: UsageWindowKindV1::new("rolling")?,
            applicable_models: BTreeSet::new(),
            used_bps: bp(10_000 - remaining)?,
            remaining_bps: bp(remaining)?,
            recent_capacity_bps: bp(8_000)?,
            resets_at_utc_ms: UtcMillisV1::new(reset_at),
            duration_ms: DurationMillisV1::new(18_000_000)?,
        }],
    })
}

fn policy(
    mode: AdaptiveEvaluationModeV1,
    unknown_usage_policy: UnknownUsagePolicyV1,
    switch_margin: u16,
) -> Result<AdaptivePolicyV1, AdaptiveRoutingError> {
    Ok(AdaptivePolicyV1 {
        policy_version: ADAPTIVE_POLICY_VERSION_V1,
        maximum_snapshot_age_ms: DurationMillisV1::new(1_000)?,
        provider_reserves: vec![
            ProviderReserveV1 {
                provider: ProviderIdV1::new("claude")?,
                usage_account_id: UsageAccountIdV1::new("primary")?,
                reserve_bps: bp(1_000)?,
            },
            ProviderReserveV1 {
                provider: ProviderIdV1::new("codex")?,
                usage_account_id: UsageAccountIdV1::new("primary")?,
                reserve_bps: bp(1_000)?,
            },
        ],
        score_weights: ScoreWeightsV1 {
            task_fit_bps: bp(5_000)?,
            usable_headroom_bps: bp(5_000)?,
            reset_proximity_bps: bp(0)?,
            recent_capacity_bps: bp(0)?,
            latency_bps: bp(0)?,
            health_bps: bp(0)?,
            configured_preference_bps: bp(0)?,
        },
        switch_margin_bps: bp(switch_margin)?,
        unknown_usage_policy,
        evaluation_mode: mode,
    })
}

fn input(
    candidates: Vec<AdaptiveCandidateV1>,
    usage_snapshots: Vec<UsageSnapshotV1>,
) -> TestResult<RoutingInputV1> {
    Ok(RoutingInputV1 {
        schema_version: ADAPTIVE_ROUTING_SCHEMA_V1,
        mission_id: MissionId::new("mission-adaptive")?,
        phase_id: PhaseId::new("phase-routing")?,
        attempt: 1,
        evaluated_at_utc_ms: UtcMillisV1::new(1_000),
        authority: RoutingAuthorityV1 {
            resolved_fixed_route: None,
            legacy_route: route("legacy", "claude", "claude")?,
        },
        requirements: RouteRequirementsV1 {
            priority: TaskPriorityV1::P1,
            required_capabilities: BTreeSet::from([CapabilityIdV1::new("tools")?]),
            minimum_quality: QualityTierV1::Standard,
        },
        candidates,
        usage_snapshots,
        incumbent_candidate_id: None,
        existing_session_runtime: None,
    })
}

fn evaluation<'a>(
    decision: &'a RoutingDecisionV1,
    candidate_id: &str,
) -> Option<&'a CandidateEvaluationV1> {
    decision
        .candidate_evaluations
        .iter()
        .find(|item| item.candidate.route.candidate_id.as_str() == candidate_id)
}

fn recommended_route(decision: &RoutingDecisionV1) -> Option<&RouteTargetV1> {
    decision.recommended_outcome.route()
}

fn dispatch_route(decision: &RoutingDecisionV1) -> Option<&RouteTargetV1> {
    decision.dispatch_outcome.route()
}

fn insert_unknown_field(
    value: &mut Value,
    pointer: &str,
    field: &str,
    unknown: Value,
) -> TestResult {
    let object = value
        .pointer_mut(pointer)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| std::io::Error::other(format!("missing object at {pointer}")))?;
    object.insert(field.to_owned(), unknown);
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureSuiteV1 {
    schema_version: u32,
    policy: AdaptivePolicyV1,
    cases: Vec<FixtureCaseV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureCaseV1 {
    name: String,
    input: RoutingInputV1,
    expected: FixtureExpectationV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureExpectationV1 {
    recommended_candidate_id: Option<String>,
    dispatch_candidate_id: Option<String>,
    deferred_reason: Option<RouteDeferralReasonV1>,
    switch_reason: SwitchReasonV1,
    continuation: ContinuationDispositionV1,
    evaluation_order: Vec<String>,
    eligible_scores: Vec<FixtureScoreV1>,
    rejections: Vec<FixtureRejectionV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureScoreV1 {
    candidate_id: String,
    score_bps: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureRejectionV1 {
    candidate_id: String,
    reason: CandidateRejectionV1,
}

#[test]
fn versioned_fixture_matrix_is_deterministic() -> TestResult {
    let fixture: FixtureSuiteV1 =
        serde_json::from_str(include_str!("fixtures/adaptive-routing-cases-v1.json"))?;
    assert_eq!(fixture.schema_version, ADAPTIVE_ROUTING_SCHEMA_V1);

    for case in fixture.cases {
        let decision = decide_route(&fixture.policy, &case.input)?;
        let mut permuted_input = case.input.clone();
        permuted_input.candidates.reverse();
        permuted_input.usage_snapshots.reverse();
        for snapshot in &mut permuted_input.usage_snapshots {
            snapshot.windows.reverse();
        }
        assert_eq!(
            decide_route(&fixture.policy, &permuted_input)?,
            decision,
            "{} input permutation",
            case.name
        );
        assert_eq!(
            recommended_route(&decision).map(|route| route.candidate_id.as_str()),
            case.expected.recommended_candidate_id.as_deref(),
            "{} recommendation",
            case.name
        );
        assert_eq!(
            dispatch_route(&decision).map(|route| route.candidate_id.as_str()),
            case.expected.dispatch_candidate_id.as_deref(),
            "{} dispatch",
            case.name
        );
        let deferred_reason = match &decision.recommended_outcome {
            RouteOutcomeV1::Deferred { reason } => Some(*reason),
            RouteOutcomeV1::Route { .. } => None,
        };
        assert_eq!(
            deferred_reason, case.expected.deferred_reason,
            "{} deferral",
            case.name
        );
        assert_eq!(
            decision.switch_reason, case.expected.switch_reason,
            "{} switch reason",
            case.name
        );
        assert_eq!(
            decision.continuation, case.expected.continuation,
            "{} continuation",
            case.name
        );
        let order = decision
            .candidate_evaluations
            .iter()
            .map(|item| item.candidate.route.candidate_id.as_str().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(order, case.expected.evaluation_order, "{} order", case.name);
        for expected in case.expected.eligible_scores {
            let found =
                evaluation(&decision, &expected.candidate_id).and_then(|item| item.score_bps);
            assert_eq!(
                found.map(BasisPointsV1::get),
                Some(expected.score_bps),
                "{} score for {}",
                case.name,
                expected.candidate_id
            );
        }
        for expected in case.expected.rejections {
            let found = evaluation(&decision, &expected.candidate_id).map(|item| item.disposition);
            assert_eq!(
                found,
                Some(CandidateDispositionV1::Rejected {
                    reason: expected.reason
                }),
                "{} rejection for {}",
                case.name,
                expected.candidate_id
            );
        }
    }
    Ok(())
}

#[test]
fn versioned_replay_fixture_recomputes_exactly() -> TestResult {
    let record: RoutingReplayRecordV1 =
        serde_json::from_str(include_str!("fixtures/adaptive-routing-replay-v1.json"))?;
    let decision = replay_route(&record)?;
    assert_eq!(&decision, record.expected_decision());
    assert_eq!(
        decision.applied_authority,
        AppliedAuthorityV1::Fixed {
            provenances: BTreeSet::from([
                FixedAuthorityProvenanceV1::AuthoredRuntime,
                FixedAuthorityProvenanceV1::ModelFlag,
            ])
        }
    );
    assert_eq!(decision.decision_mode, RoutingDecisionModeV1::Fixed);
    Ok(())
}

#[test]
fn hand_authored_adaptive_replay_fixture_recomputes_exactly() -> TestResult {
    let record: RoutingReplayRecordV1 = serde_json::from_str(include_str!(
        "fixtures/adaptive-routing-adaptive-replay-v1.json"
    ))?;
    let replayed = replay_route(&record)?;
    assert_eq!(&replayed, record.expected_decision());
    assert_eq!(
        replayed
            .applied_policy
            .as_ref()
            .map(|policy| policy.policy_version),
        Some(ADAPTIVE_POLICY_VERSION_V1)
    );
    assert_eq!(
        replayed.decision_mode,
        RoutingDecisionModeV1::Adaptive {
            evaluation_mode: AdaptiveEvaluationModeV1::Enforce
        }
    );
    let winner = evaluation(&replayed, "adaptive-replay")
        .ok_or_else(|| std::io::Error::other("missing eligible replay winner"))?;
    assert_eq!(winner.disposition, CandidateDispositionV1::Eligible);
    assert_eq!(
        winner.considered_snapshot_ids,
        BTreeSet::from([SnapshotIdV1::new("adaptive-replay-usage")?])
    );
    assert_eq!(
        winner.governing_snapshot_ids,
        winner.considered_snapshot_ids
    );
    let loser = evaluation(&replayed, "rejected-loser")
        .ok_or_else(|| std::io::Error::other("missing rejected replay loser"))?;
    assert_eq!(
        loser.disposition,
        CandidateDispositionV1::Rejected {
            reason: CandidateRejectionV1::Exhausted
        }
    );
    assert_eq!(
        loser.considered_snapshot_ids,
        BTreeSet::from([SnapshotIdV1::new("rejected-loser-usage")?])
    );
    assert_eq!(loser.governing_snapshot_ids, loser.considered_snapshot_ids);
    Ok(())
}

#[test]
fn fixed_authority_precedes_invalid_adaptive_policy_legacy_and_payload() -> TestResult {
    let mut routing_input = input(Vec::new(), Vec::new())?;
    routing_input.authority.resolved_fixed_route = Some(FixedRouteV1 {
        composition_version: ADAPTIVE_ROUTING_SCHEMA_V1,
        provenances: BTreeSet::from([
            FixedAuthorityProvenanceV1::AuthoredRuntime,
            FixedAuthorityProvenanceV1::ModelFlag,
        ]),
        route: route("authored-route", "claude", "claude")?,
    });
    let mut ignored_snapshot = snapshot("ignored", "codex", 0, 900, 1_000, 2_000)?;
    ignored_snapshot.schema_version = 99;
    routing_input.usage_snapshots = vec![ignored_snapshot];
    routing_input.candidates = vec![AdaptiveCandidateV1 {
        route: RouteTargetV1 {
            model: String::new(),
            ..route("ignored-candidate", "codex", "codex")?
        },
        ..candidate("ignored-candidate", "codex", "codex", 10_000)?
    }];
    routing_input.authority.legacy_route.model.clear();
    let mut ignored_policy = policy(
        AdaptiveEvaluationModeV1::Shadow,
        UnknownUsagePolicyV1::StaticFallback,
        500,
    )?;
    ignored_policy.policy_version = 99;
    ignored_policy.score_weights.health_bps = bp(1)?;
    let duplicate_reserve = ignored_policy.provider_reserves[0].clone();
    ignored_policy.provider_reserves.push(duplicate_reserve);

    let decision = decide_route(&ignored_policy, &routing_input)?;
    assert_eq!(
        recommended_route(&decision).map(|route| route.candidate_id.as_str()),
        Some("authored-route")
    );
    assert_eq!(
        dispatch_route(&decision).map(|route| route.candidate_id.as_str()),
        Some("authored-route")
    );
    assert_eq!(decision.applied_policy, None);
    assert_eq!(decision.decision_mode, RoutingDecisionModeV1::Fixed);
    assert_eq!(decision.legacy_route, None);
    assert!(decision.candidate_evaluations.is_empty());
    assert!(decision.referenced_snapshots.is_empty());
    Ok(())
}

#[test]
fn fixed_authority_still_validates_selected_route_and_request_invariants() -> TestResult {
    let mut routing_input = input(Vec::new(), Vec::new())?;
    routing_input.authority.resolved_fixed_route = Some(FixedRouteV1 {
        composition_version: ADAPTIVE_ROUTING_SCHEMA_V1,
        provenances: BTreeSet::from([FixedAuthorityProvenanceV1::ModelFlag]),
        route: RouteTargetV1 {
            model: String::new(),
            ..route("authored-route", "claude", "claude")?
        },
    });
    assert!(matches!(
        decide_route(
            &policy(
                AdaptiveEvaluationModeV1::Enforce,
                UnknownUsagePolicyV1::StaticFallback,
                500
            )?,
            &routing_input
        ),
        Err(AdaptiveRoutingError::InvalidText {
            field: "route.model",
            ..
        })
    ));

    routing_input.authority.resolved_fixed_route = Some(FixedRouteV1 {
        composition_version: ADAPTIVE_ROUTING_SCHEMA_V1,
        provenances: BTreeSet::from([FixedAuthorityProvenanceV1::ModelFlag]),
        route: route("authored-route", "claude", "claude")?,
    });
    routing_input.attempt = 0;
    assert!(matches!(
        decide_route(
            &policy(
                AdaptiveEvaluationModeV1::Enforce,
                UnknownUsagePolicyV1::StaticFallback,
                500
            )?,
            &routing_input
        ),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "routing_input.attempt",
            ..
        })
    ));
    Ok(())
}

#[test]
fn provider_reserve_permutations_produce_canonical_decision_and_replay_bytes() -> TestResult {
    let routing_input = input(
        vec![candidate("canonical-policy", "codex", "codex", 8_000)?],
        vec![snapshot(
            "canonical-policy-usage",
            "codex",
            9_000,
            900,
            1_000,
            2_000,
        )?],
    )?;
    let mut sorted_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::Defer,
        500,
    )?;
    sorted_policy.provider_reserves.push(ProviderReserveV1 {
        provider: ProviderIdV1::new("codex")?,
        usage_account_id: UsageAccountIdV1::new("secondary")?,
        reserve_bps: bp(2_000)?,
    });
    let mut reversed_policy = sorted_policy.clone();
    reversed_policy.provider_reserves.reverse();

    let sorted = decide_route(&sorted_policy, &routing_input)?;
    let reversed = decide_route(&reversed_policy, &routing_input)?;
    assert_eq!(reversed, sorted);
    assert_eq!(
        reversed.applied_policy.as_ref().map(|policy| {
            policy
                .provider_reserves
                .iter()
                .map(|reserve| (reserve.provider.as_str(), reserve.usage_account_id.as_str()))
                .collect::<Vec<_>>()
        }),
        Some(vec![
            ("claude", "primary"),
            ("codex", "primary"),
            ("codex", "secondary"),
        ])
    );

    let sorted_record = RoutingReplayRecordV1::new(sorted_policy, routing_input.clone(), sorted)?;
    let reversed_record = RoutingReplayRecordV1::new(reversed_policy, routing_input, reversed)?;
    assert_eq!(
        serde_json::to_vec(&sorted_record)?,
        serde_json::to_vec(&reversed_record)?
    );
    assert_eq!(sorted_record.schema_version(), ADAPTIVE_ROUTING_SCHEMA_V1);

    let mut noncanonical_wire = serde_json::to_value(&sorted_record)?;
    noncanonical_wire
        .pointer_mut("/policy/provider_reserves")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| std::io::Error::other("missing replay policy reserves"))?
        .reverse();
    assert!(serde_json::from_value::<RoutingReplayRecordV1>(noncanonical_wire).is_err());
    Ok(())
}

#[test]
fn automatic_authorization_capabilities_and_quality_are_hard_constraints() -> TestResult {
    let mut advisory = candidate("advisory", "codex", "codex", 10_000)?;
    advisory.automatic_eligibility = AutomaticEligibilityV1::AdvisoryOnly;
    let mut explicit = candidate("explicit", "codex", "codex", 10_000)?;
    explicit.automatic_eligibility = AutomaticEligibilityV1::ExplicitOnly;
    let mut missing_capability = candidate("missing-cap", "codex", "codex", 10_000)?;
    missing_capability.capabilities.clear();
    let mut low_quality = candidate("low-quality", "codex", "codex", 10_000)?;
    low_quality.quality = QualityTierV1::Economy;
    let routing_input = input(
        vec![low_quality, missing_capability, explicit, advisory],
        Vec::new(),
    )?;
    let decision = decide_route(
        &policy(
            AdaptiveEvaluationModeV1::Enforce,
            UnknownUsagePolicyV1::StaticFallback,
            500,
        )?,
        &routing_input,
    )?;

    let expected = [
        ("advisory", CandidateRejectionV1::AdvisoryOnly),
        ("explicit", CandidateRejectionV1::ExplicitOnly),
        ("low-quality", CandidateRejectionV1::BelowQualityFloor),
        ("missing-cap", CandidateRejectionV1::MissingCapability),
    ];
    for (candidate_id, reason) in expected {
        let found = evaluation(&decision, candidate_id);
        assert_eq!(
            found.map(|item| item.disposition),
            Some(CandidateDispositionV1::Rejected { reason })
        );
        assert!(found.is_some_and(|item| item.governing_snapshot_ids.is_empty()));
        assert_eq!(found.and_then(|item| item.score_bps), None);
    }
    assert_eq!(
        decision.dispatch_outcome,
        RouteOutcomeV1::Deferred {
            reason: RouteDeferralReasonV1::HardConstraint
        }
    );
    assert_eq!(decision.continuation, ContinuationDispositionV1::NoDispatch);
    Ok(())
}

#[test]
fn reserve_boundary_is_protected_but_p0_may_consume_it() -> TestResult {
    let route_candidate = candidate("critical", "codex", "codex", 7_000)?;
    let usage = snapshot("reserve-edge", "codex", 1_000, 900, 1_000, 2_000)?;
    let mut routing_input = input(vec![route_candidate], vec![usage])?;
    let routing_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::StaticFallback,
        500,
    )?;

    let ordinary = decide_route(&routing_policy, &routing_input)?;
    assert_eq!(
        evaluation(&ordinary, "critical").map(|item| item.disposition),
        Some(CandidateDispositionV1::Rejected {
            reason: CandidateRejectionV1::ReserveProtected
        })
    );
    assert_eq!(
        ordinary.dispatch_outcome,
        RouteOutcomeV1::Deferred {
            reason: RouteDeferralReasonV1::CapacityUnavailable
        }
    );

    routing_input.requirements.priority = TaskPriorityV1::P0;
    let critical = decide_route(&routing_policy, &routing_input)?;
    assert_eq!(
        dispatch_route(&critical).map(|route| route.candidate_id.as_str()),
        Some("critical")
    );
    assert_eq!(
        evaluation(&critical, "critical").and_then(|item| item.usable_headroom_bps),
        Some(bp(1_000)?)
    );
    assert_eq!(
        evaluation(&critical, "critical").and_then(|item| item.reported_remaining_bps),
        Some(bp(1_000)?)
    );
    assert_eq!(
        evaluation(&critical, "critical").and_then(|item| item.applied_reserve_bps),
        Some(bp(1_000)?)
    );
    assert!(
        evaluation(&critical, "critical")
            .and_then(|item| item.score_components)
            .is_some()
    );
    assert_eq!(critical.mission_id.as_str(), "mission-adaptive");
    assert_eq!(critical.phase_id.as_str(), "phase-routing");
    assert_eq!(critical.attempt, 1);
    Ok(())
}

#[test]
fn ttl_reset_and_unknown_usage_fail_closed() -> TestResult {
    let route_candidate = candidate("incumbent", "codex", "codex", 7_000)?;
    let mut stale_input = input(
        vec![route_candidate.clone()],
        vec![snapshot("stale", "codex", 8_000, 0, 1_000, 2_000)?],
    )?;
    stale_input.incumbent_candidate_id = Some(CandidateIdV1::new("incumbent")?);
    let static_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::StaticFallback,
        500,
    )?;
    let stale = decide_route(&static_policy, &stale_input)?;
    assert_eq!(
        evaluation(&stale, "incumbent").map(|item| item.disposition),
        Some(CandidateDispositionV1::Rejected {
            reason: CandidateRejectionV1::StaleUsageSnapshot
        })
    );
    assert_eq!(
        stale.dispatch_outcome,
        RouteOutcomeV1::Deferred {
            reason: RouteDeferralReasonV1::UnknownUsage
        }
    );

    let mut reset_input = stale_input.clone();
    reset_input.usage_snapshots = vec![snapshot("reset", "codex", 8_000, 900, 1_000, 1_000)?];
    let reset = decide_route(&static_policy, &reset_input)?;
    assert_eq!(
        evaluation(&reset, "incumbent").map(|item| item.disposition),
        Some(CandidateDispositionV1::Rejected {
            reason: CandidateRejectionV1::ResetElapsed
        })
    );

    let mut wrong_account_snapshot = snapshot("wrong-account", "codex", 8_000, 900, 1_000, 2_000)?;
    wrong_account_snapshot.usage_account_id = UsageAccountIdV1::new("secondary")?;
    let wrong_account_input = input(vec![route_candidate.clone()], vec![wrong_account_snapshot])?;
    let wrong_account = decide_route(&static_policy, &wrong_account_input)?;
    assert_eq!(
        evaluation(&wrong_account, "incumbent").map(|item| item.disposition),
        Some(CandidateDispositionV1::Rejected {
            reason: CandidateRejectionV1::MissingUsageSnapshot
        })
    );

    let mut unknown_input = input(vec![route_candidate], Vec::new())?;
    unknown_input.incumbent_candidate_id = Some(CandidateIdV1::new("incumbent")?);
    let hold = decide_route(
        &policy(
            AdaptiveEvaluationModeV1::Enforce,
            UnknownUsagePolicyV1::HoldIncumbent,
            500,
        )?,
        &unknown_input,
    )?;
    assert_eq!(
        dispatch_route(&hold).map(|route| route.candidate_id.as_str()),
        Some("incumbent")
    );
    assert_eq!(
        hold.switch_reason,
        SwitchReasonV1::IncumbentHeldOnUnknownUsage {
            rejection: CandidateRejectionV1::MissingUsageSnapshot
        }
    );
    Ok(())
}

#[test]
fn canonical_legacy_fallback_is_limited_to_unknown_usage() -> TestResult {
    let mut forbidden = candidate("forbidden", "codex", "codex", 7_000)?;
    forbidden.automatic_eligibility = AutomaticEligibilityV1::ExplicitOnly;
    let mut forbidden_input = input(vec![forbidden.clone()], Vec::new())?;
    forbidden_input.authority.legacy_route = RouteTargetV1 {
        candidate_id: CandidateIdV1::new("legacy-alias")?,
        ..forbidden.route.clone()
    };
    let routing_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::StaticFallback,
        500,
    )?;
    let forbidden_decision = decide_route(&routing_policy, &forbidden_input)?;
    assert_eq!(
        forbidden_decision.dispatch_outcome,
        RouteOutcomeV1::Deferred {
            reason: RouteDeferralReasonV1::HardConstraint
        }
    );

    let exhausted_candidate = candidate("exhausted", "codex", "codex", 7_000)?;
    let mut exhausted_input = input(
        vec![exhausted_candidate.clone()],
        vec![snapshot("exhausted-usage", "codex", 0, 900, 1_000, 2_000)?],
    )?;
    exhausted_input.authority.legacy_route = RouteTargetV1 {
        candidate_id: CandidateIdV1::new("exhausted-alias")?,
        ..exhausted_candidate.route.clone()
    };
    let exhausted_decision = decide_route(&routing_policy, &exhausted_input)?;
    assert_eq!(
        exhausted_decision.dispatch_outcome,
        RouteOutcomeV1::Deferred {
            reason: RouteDeferralReasonV1::CapacityUnavailable
        }
    );

    let unknown_candidate = candidate("unknown", "codex", "codex", 7_000)?;
    let mut unknown_input = input(vec![unknown_candidate.clone()], Vec::new())?;
    unknown_input.authority.legacy_route = RouteTargetV1 {
        candidate_id: CandidateIdV1::new("unknown-alias")?,
        ..unknown_candidate.route.clone()
    };
    let unknown_decision = decide_route(&routing_policy, &unknown_input)?;
    assert_eq!(
        dispatch_route(&unknown_decision).map(|route| route.candidate_id.as_str()),
        Some("unknown")
    );
    assert_eq!(
        unknown_decision.switch_reason,
        SwitchReasonV1::LegacyFallbackOnUnknownUsage
    );
    Ok(())
}

#[test]
fn observation_age_and_unhealthy_empty_snapshots_fail_closed() -> TestResult {
    let route_candidate = candidate("observed-age", "codex", "codex", 7_000)?;
    let mut delayed = snapshot("delayed", "codex", 8_000, 900, 1_000, 2_000)?;
    delayed.observed_at_utc_ms = UtcMillisV1::new(0);
    let delayed_decision = decide_route(
        &policy(
            AdaptiveEvaluationModeV1::Enforce,
            UnknownUsagePolicyV1::StaticFallback,
            500,
        )?,
        &input(vec![route_candidate.clone()], vec![delayed])?,
    )?;
    assert_eq!(
        evaluation(&delayed_decision, "observed-age").map(|item| item.disposition),
        Some(CandidateDispositionV1::Rejected {
            reason: CandidateRejectionV1::StaleUsageSnapshot
        })
    );

    let mut unavailable = snapshot("unavailable", "codex", 8_000, 900, 1_000, 2_000)?;
    unavailable.health = UsageHealthV1::Unavailable;
    unavailable.confidence_bps = bp(0)?;
    unavailable.reason_code = Some(UsageReasonCodeV1::new("provider_unavailable")?);
    unavailable.windows.clear();
    let unavailable_decision = decide_route(
        &policy(
            AdaptiveEvaluationModeV1::Enforce,
            UnknownUsagePolicyV1::StaticFallback,
            500,
        )?,
        &input(vec![route_candidate], vec![unavailable.clone()])?,
    )?;
    assert_eq!(
        evaluation(&unavailable_decision, "observed-age").map(|item| item.disposition),
        Some(CandidateDispositionV1::Rejected {
            reason: CandidateRejectionV1::UnhealthyUsage
        })
    );
    assert_eq!(
        evaluation(&unavailable_decision, "observed-age")
            .and_then(|item| item.usage_reason_code.as_ref())
            .map(UsageReasonCodeV1::as_str),
        Some("provider_unavailable")
    );

    unavailable.reason_code = None;
    assert!(matches!(
        decide_route(
            &policy(
                AdaptiveEvaluationModeV1::Enforce,
                UnknownUsagePolicyV1::StaticFallback,
                500
            )?,
            &input(
                vec![candidate("unhealthy-no-reason", "codex", "codex", 7_000)?],
                vec![unavailable]
            )?
        ),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "usage_snapshot.reason_code",
            ..
        })
    ));
    Ok(())
}

#[test]
fn newest_provider_observation_wins_over_later_receipt_of_older_data() -> TestResult {
    let freshest = snapshot("fresh-observation", "codex", 2_000, 950, 1_000, 2_000)?;
    let mut delayed_older = snapshot("delayed-older", "codex", 9_000, 960, 1_000, 2_000)?;
    delayed_older.observed_at_utc_ms = UtcMillisV1::new(900);
    let routing_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::StaticFallback,
        500,
    )?;
    let routing_input = input(
        vec![candidate("ordered", "codex", "codex", 7_000)?],
        vec![delayed_older, freshest],
    )?;

    let decision = decide_route(&routing_policy, &routing_input)?;
    let selected = evaluation(&decision, "ordered");
    assert_eq!(
        selected
            .and_then(|item| item.governing_snapshot_ids.iter().next())
            .map(SnapshotIdV1::as_str),
        Some("fresh-observation")
    );
    assert_eq!(
        selected.and_then(|item| item.reported_remaining_bps),
        Some(bp(2_000)?)
    );

    let mut permuted = routing_input;
    permuted.usage_snapshots.reverse();
    assert_eq!(decide_route(&routing_policy, &permuted)?, decision);
    Ok(())
}

#[test]
fn stable_ties_and_hysteresis_are_independent_of_input_order() -> TestResult {
    let incumbent = candidate("incumbent", "codex", "codex", 5_000)?;
    let challenger_below = candidate("challenger", "claude", "claude", 5_499)?;
    let mut routing_input = input(
        vec![challenger_below, incumbent.clone()],
        vec![
            snapshot("claude-usage", "claude", 9_000, 900, 1_000, 2_000)?,
            snapshot("codex-usage", "codex", 9_000, 900, 1_000, 2_000)?,
        ],
    )?;
    routing_input.incumbent_candidate_id = Some(CandidateIdV1::new("incumbent")?);
    let mut routing_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::StaticFallback,
        500,
    )?;
    routing_policy.score_weights = ScoreWeightsV1 {
        task_fit_bps: bp(10_000)?,
        usable_headroom_bps: bp(0)?,
        reset_proximity_bps: bp(0)?,
        recent_capacity_bps: bp(0)?,
        latency_bps: bp(0)?,
        health_bps: bp(0)?,
        configured_preference_bps: bp(0)?,
    };

    let below = decide_route(&routing_policy, &routing_input)?;
    assert_eq!(
        dispatch_route(&below).map(|route| route.candidate_id.as_str()),
        Some("incumbent")
    );
    assert_eq!(
        below.switch_reason,
        SwitchReasonV1::IncumbentHeldByHysteresis
    );

    routing_input.candidates[0].task_fit_bps = bp(5_500)?;
    let exact = decide_route(&routing_policy, &routing_input)?;
    assert_eq!(
        dispatch_route(&exact).map(|route| route.candidate_id.as_str()),
        Some("challenger")
    );
    assert_eq!(exact.switch_reason, SwitchReasonV1::ChallengerMetMargin);

    routing_input.candidates.reverse();
    routing_input.usage_snapshots.reverse();
    let permuted = decide_route(&routing_policy, &routing_input)?;
    assert_eq!(permuted, exact);

    let mut hard_ineligible = routing_input;
    let incumbent_route = hard_ineligible
        .candidates
        .iter_mut()
        .find(|item| item.route.candidate_id.as_str() == "incumbent");
    if let Some(candidate) = incumbent_route {
        candidate.automatic_eligibility = AutomaticEligibilityV1::AdvisoryOnly;
    }
    let switched = decide_route(&routing_policy, &hard_ineligible)?;
    assert_eq!(
        dispatch_route(&switched).map(|route| route.candidate_id.as_str()),
        Some("challenger")
    );
    assert_eq!(
        switched.switch_reason,
        SwitchReasonV1::IncumbentIneligible {
            rejection: CandidateRejectionV1::AdvisoryOnly
        }
    );
    Ok(())
}

#[test]
fn incumbent_replacement_records_the_exact_capacity_rejection() -> TestResult {
    let mut routing_input = input(
        vec![
            candidate("incumbent", "codex", "codex", 10_000)?,
            candidate("usable-challenger", "claude", "claude", 5_000)?,
        ],
        vec![
            snapshot("incumbent-empty", "codex", 0, 900, 1_000, 2_000)?,
            snapshot("challenger-usage", "claude", 9_000, 900, 1_000, 2_000)?,
        ],
    )?;
    routing_input.incumbent_candidate_id = Some(CandidateIdV1::new("incumbent")?);
    let decision = decide_route(
        &policy(
            AdaptiveEvaluationModeV1::Enforce,
            UnknownUsagePolicyV1::Defer,
            500,
        )?,
        &routing_input,
    )?;
    assert_eq!(
        decision.switch_reason,
        SwitchReasonV1::IncumbentIneligible {
            rejection: CandidateRejectionV1::Exhausted
        }
    );
    assert_eq!(
        dispatch_route(&decision).map(|route| route.candidate_id.as_str()),
        Some("usable-challenger")
    );
    let mut wire = serde_json::to_value(&decision)?;
    insert_unknown_field(
        &mut wire,
        "/switch_reason/incumbent_ineligible",
        "untyped_detail",
        serde_json::json!("exhausted"),
    )?;
    assert!(serde_json::from_value::<RoutingDecisionV1>(wire).is_err());
    Ok(())
}

#[test]
fn stale_and_conflicting_incumbents_record_typed_replayable_rejections() -> TestResult {
    let mut stale_input = input(
        vec![
            candidate("incumbent", "codex", "codex", 10_000)?,
            candidate("usable-challenger", "claude", "claude", 5_000)?,
        ],
        vec![
            snapshot("incumbent-stale", "codex", 9_000, 0, 1_000, 2_000)?,
            snapshot("challenger-stale-case", "claude", 9_000, 900, 1_000, 2_000)?,
        ],
    )?;
    stale_input.incumbent_candidate_id = Some(CandidateIdV1::new("incumbent")?);

    let mut conflict_input = input(
        vec![
            candidate("incumbent", "codex", "codex", 10_000)?,
            candidate("usable-challenger", "claude", "claude", 5_000)?,
        ],
        vec![
            snapshot("incumbent-conflict-a", "codex", 9_000, 900, 1_000, 2_000)?,
            snapshot("incumbent-conflict-b", "codex", 8_000, 900, 1_000, 2_000)?,
            snapshot(
                "challenger-conflict-case",
                "claude",
                9_000,
                900,
                1_000,
                2_000,
            )?,
        ],
    )?;
    conflict_input.incumbent_candidate_id = Some(CandidateIdV1::new("incumbent")?);

    let routing_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::Defer,
        500,
    )?;
    for (rejection, governing_count, routing_input) in [
        (CandidateRejectionV1::StaleUsageSnapshot, 1, stale_input),
        (
            CandidateRejectionV1::ConflictingUsageSnapshots,
            2,
            conflict_input,
        ),
    ] {
        let decision = decide_route(&routing_policy, &routing_input)?;
        assert_eq!(
            decision.switch_reason,
            SwitchReasonV1::IncumbentIneligible { rejection }
        );
        assert_eq!(
            dispatch_route(&decision).map(|route| route.candidate_id.as_str()),
            Some("usable-challenger")
        );
        let incumbent = evaluation(&decision, "incumbent")
            .ok_or_else(|| std::io::Error::other("missing incumbent evaluation"))?;
        assert_eq!(
            incumbent.disposition,
            CandidateDispositionV1::Rejected { reason: rejection }
        );
        assert_eq!(incumbent.governing_snapshot_ids.len(), governing_count);

        let record =
            RoutingReplayRecordV1::new(routing_policy.clone(), routing_input, decision.clone())?;
        let serialized = serde_json::to_vec(&record)?;
        let decoded: RoutingReplayRecordV1 = serde_json::from_slice(&serialized)?;
        assert_eq!(decoded, record);
        assert_eq!(replay_route(&decoded)?, decision);

        let mut invalid_shape = serde_json::to_value(&record)?;
        *invalid_shape
            .pointer_mut("/expected_decision/switch_reason/incumbent_ineligible/rejection")
            .ok_or_else(|| std::io::Error::other("missing typed incumbent replay reason"))? =
            serde_json::json!("exhausted");
        assert!(serde_json::from_value::<RoutingReplayRecordV1>(invalid_shape).is_err());
    }
    Ok(())
}

#[test]
fn equal_scores_prefer_the_existing_runtime_before_lexical_id() -> TestResult {
    let mut routing_input = input(
        vec![
            candidate("alpha-foreign", "claude", "claude", 5_000)?,
            candidate("zeta-local", "codex", "codex", 5_000)?,
        ],
        vec![
            snapshot("claude-equal", "claude", 9_000, 900, 1_000, 2_000)?,
            snapshot("codex-equal", "codex", 9_000, 900, 1_000, 2_000)?,
        ],
    )?;
    routing_input.existing_session_runtime = Some(RuntimeIdV1::new("codex")?);
    let mut routing_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::StaticFallback,
        0,
    )?;
    routing_policy.score_weights = ScoreWeightsV1 {
        task_fit_bps: bp(10_000)?,
        usable_headroom_bps: bp(0)?,
        reset_proximity_bps: bp(0)?,
        recent_capacity_bps: bp(0)?,
        latency_bps: bp(0)?,
        health_bps: bp(0)?,
        configured_preference_bps: bp(0)?,
    };

    let local = decide_route(&routing_policy, &routing_input)?;
    assert_eq!(
        dispatch_route(&local).map(|route| route.candidate_id.as_str()),
        Some("zeta-local")
    );
    assert_eq!(
        local.continuation,
        ContinuationDispositionV1::SameRuntimeMayResume
    );

    routing_input.existing_session_runtime = None;
    let lexical = decide_route(&routing_policy, &routing_input)?;
    assert_eq!(
        dispatch_route(&lexical).map(|route| route.candidate_id.as_str()),
        Some("alpha-foreign")
    );
    Ok(())
}

#[test]
fn reset_proximity_and_recent_capacity_are_explicit_score_components() -> TestResult {
    let mut soon_reset = snapshot("soon-usage", "codex", 8_000, 900, 1_000, 1_100)?;
    soon_reset.windows[0].recent_capacity_bps = bp(2_000)?;
    let mut far_reset = snapshot("far-usage", "claude", 8_000, 900, 1_000, 18_000_900)?;
    far_reset.windows[0].recent_capacity_bps = bp(9_000)?;
    let routing_input = input(
        vec![
            candidate("far", "claude", "claude", 5_000)?,
            candidate("soon", "codex", "codex", 5_000)?,
        ],
        vec![far_reset, soon_reset],
    )?;
    let mut routing_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::StaticFallback,
        0,
    )?;
    routing_policy.score_weights = ScoreWeightsV1 {
        task_fit_bps: bp(0)?,
        usable_headroom_bps: bp(0)?,
        reset_proximity_bps: bp(5_000)?,
        recent_capacity_bps: bp(5_000)?,
        latency_bps: bp(0)?,
        health_bps: bp(0)?,
        configured_preference_bps: bp(0)?,
    };

    let decision = decide_route(&routing_policy, &routing_input)?;
    assert_eq!(
        dispatch_route(&decision).map(|route| route.candidate_id.as_str()),
        Some("soon")
    );
    assert_eq!(
        evaluation(&decision, "soon").and_then(|item| item.score_bps),
        Some(bp(6_000)?)
    );
    assert_eq!(
        evaluation(&decision, "far").and_then(|item| item.score_bps),
        Some(bp(4_500)?)
    );

    let mut multiple_windows = snapshot("multi-window", "codex", 8_000, 900, 1_000, 1_100)?;
    multiple_windows.windows.push(UsageWindowV1 {
        limit_id: UsageLimitIdV1::new("weekly")?,
        kind: UsageWindowKindV1::new("weekly")?,
        applicable_models: BTreeSet::new(),
        used_bps: bp(2_000)?,
        remaining_bps: bp(8_000)?,
        recent_capacity_bps: bp(8_000)?,
        resets_at_utc_ms: UtcMillisV1::new(604_800_900),
        duration_ms: DurationMillisV1::new(604_800_000)?,
    });
    let multiple_window_input = input(
        vec![candidate("multi", "codex", "codex", 5_000)?],
        vec![multiple_windows],
    )?;
    let multiple_window_decision = decide_route(&routing_policy, &multiple_window_input)?;
    assert_eq!(
        evaluation(&multiple_window_decision, "multi")
            .and_then(|item| item.score_components)
            .map(|components| components.reset_proximity_bps),
        Some(bp(1)?)
    );
    let mut permuted_windows = multiple_window_input;
    permuted_windows.usage_snapshots[0].windows.reverse();
    assert_eq!(
        decide_route(&routing_policy, &permuted_windows)?,
        multiple_window_decision
    );
    Ok(())
}

#[test]
fn shadow_dispatch_and_session_disposition_follow_dispatched_runtime() -> TestResult {
    let route_candidate = candidate("codex-best", "codex", "codex", 9_000)?;
    let usage = snapshot("codex-fresh", "codex", 9_000, 900, 1_000, 2_000)?;
    let mut routing_input = input(vec![route_candidate], vec![usage])?;
    routing_input.existing_session_runtime = Some(RuntimeIdV1::new("claude")?);

    let shadow = decide_route(
        &policy(
            AdaptiveEvaluationModeV1::Shadow,
            UnknownUsagePolicyV1::StaticFallback,
            500,
        )?,
        &routing_input,
    )?;
    assert_eq!(
        recommended_route(&shadow).map(|route| route.candidate_id.as_str()),
        Some("codex-best")
    );
    assert_eq!(
        dispatch_route(&shadow).map(|route| route.candidate_id.as_str()),
        Some("legacy")
    );
    assert_eq!(
        shadow.continuation,
        ContinuationDispositionV1::SameRuntimeMayResume
    );
    assert_eq!(
        shadow.decision_mode,
        RoutingDecisionModeV1::Adaptive {
            evaluation_mode: AdaptiveEvaluationModeV1::Shadow
        }
    );

    let enforced = decide_route(
        &policy(
            AdaptiveEvaluationModeV1::Enforce,
            UnknownUsagePolicyV1::StaticFallback,
            500,
        )?,
        &routing_input,
    )?;
    assert_eq!(
        enforced.continuation,
        ContinuationDispositionV1::FreshSessionRequired
    );
    assert_eq!(
        enforced.decision_mode,
        RoutingDecisionModeV1::Adaptive {
            evaluation_mode: AdaptiveEvaluationModeV1::Enforce
        }
    );
    Ok(())
}

#[test]
fn unknown_fields_fail_closed_across_policy_input_snapshot_decision_and_replay() -> TestResult {
    let routing_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::StaticFallback,
        500,
    )?;
    let routing_input = input(
        vec![candidate("strict-wire", "codex", "codex", 8_000)?],
        vec![snapshot(
            "strict-wire-usage",
            "codex",
            9_000,
            900,
            1_000,
            2_000,
        )?],
    )?;
    let decision = decide_route(&routing_policy, &routing_input)?;
    let record = RoutingReplayRecordV1::new(
        routing_policy.clone(),
        routing_input.clone(),
        decision.clone(),
    )?;

    let mut policy_wire = serde_json::to_value(&routing_policy)?;
    insert_unknown_field(
        &mut policy_wire,
        "",
        "minimum_remaining_bps",
        serde_json::json!(500),
    )?;
    assert!(serde_json::from_value::<AdaptivePolicyV1>(policy_wire).is_err());

    let mut nested_policy_wire = serde_json::to_value(&routing_policy)?;
    insert_unknown_field(
        &mut nested_policy_wire,
        "/score_weights",
        "unversioned_weight",
        serde_json::json!(1),
    )?;
    assert!(serde_json::from_value::<AdaptivePolicyV1>(nested_policy_wire).is_err());

    let mut input_wire = serde_json::to_value(&routing_input)?;
    insert_unknown_field(
        &mut input_wire,
        "",
        "implicit_candidate",
        serde_json::json!(true),
    )?;
    assert!(serde_json::from_value::<RoutingInputV1>(input_wire).is_err());

    let mut route_wire = serde_json::to_value(&routing_input)?;
    insert_unknown_field(
        &mut route_wire,
        "/authority/legacy_route",
        "implicit_model",
        serde_json::json!("unsafe"),
    )?;
    assert!(serde_json::from_value::<RoutingInputV1>(route_wire).is_err());

    let mut snapshot_wire = serde_json::to_value(&routing_input)?;
    insert_unknown_field(
        &mut snapshot_wire,
        "/usage_snapshots/0",
        "raw_payload",
        serde_json::json!({"credential": "must-not-enter-core"}),
    )?;
    assert!(serde_json::from_value::<RoutingInputV1>(snapshot_wire).is_err());

    let mut decision_wire = serde_json::to_value(&decision)?;
    insert_unknown_field(
        &mut decision_wire,
        "",
        "implicit_dispatch",
        serde_json::json!(true),
    )?;
    assert!(serde_json::from_value::<RoutingDecisionV1>(decision_wire).is_err());

    let mut decision_mode_wire = serde_json::to_value(&decision)?;
    insert_unknown_field(
        &mut decision_mode_wire,
        "/decision_mode",
        "unapplied_mode",
        serde_json::json!("fixed"),
    )?;
    assert!(serde_json::from_value::<RoutingDecisionV1>(decision_mode_wire).is_err());

    let mut outcome_wire = serde_json::to_value(&decision)?;
    insert_unknown_field(
        &mut outcome_wire,
        "/dispatch_outcome",
        "unverified_outcome_field",
        serde_json::json!(true),
    )?;
    assert!(serde_json::from_value::<RoutingDecisionV1>(outcome_wire).is_err());

    let mut outcome_route_wire = serde_json::to_value(&decision)?;
    insert_unknown_field(
        &mut outcome_route_wire,
        "/dispatch_outcome/route",
        "unverified_override",
        serde_json::json!(true),
    )?;
    assert!(serde_json::from_value::<RoutingDecisionV1>(outcome_route_wire).is_err());

    let mut replay_wire = serde_json::to_value(&record)?;
    insert_unknown_field(
        &mut replay_wire,
        "",
        "unversioned_extension",
        serde_json::json!(true),
    )?;
    assert!(serde_json::from_value::<RoutingReplayRecordV1>(replay_wire).is_err());

    let mut fixture_wire: Value =
        serde_json::from_str(include_str!("fixtures/adaptive-routing-cases-v1.json"))?;
    insert_unknown_field(
        &mut fixture_wire,
        "/cases/0/expected",
        "implicit_expectation",
        serde_json::json!(true),
    )?;
    assert!(serde_json::from_value::<FixtureSuiteV1>(fixture_wire).is_err());
    Ok(())
}

#[test]
fn versions_integer_bounds_and_replay_mismatch_fail_closed() -> TestResult {
    assert!(serde_json::from_str::<BasisPointsV1>("10001").is_err());
    assert!(serde_json::from_str::<DurationMillisV1>("0").is_err());
    assert!(
        serde_json::from_str::<DurationMillisV1>(&(ADAPTIVE_MAX_DURATION_MS_V1 + 1).to_string())
            .is_err()
    );
    assert!(serde_json::from_str::<BasisPointsV1>("NaN").is_err());
    assert!(serde_json::from_str::<BasisPointsV1>("Infinity").is_err());
    assert!(serde_json::from_str::<BasisPointsV1>("1.5").is_err());

    let routing_input = input(Vec::new(), Vec::new())?;
    let mut routing_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::StaticFallback,
        500,
    )?;
    routing_policy.policy_version = 2;
    assert_eq!(
        decide_route(&routing_policy, &routing_input),
        Err(AdaptiveRoutingError::UnsupportedPolicyVersion {
            found: 2,
            expected: ADAPTIVE_POLICY_VERSION_V1
        })
    );

    let mut invalid_weights = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::StaticFallback,
        500,
    )?;
    invalid_weights.score_weights.health_bps = bp(1)?;
    assert!(matches!(
        decide_route(&invalid_weights, &routing_input),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "adaptive_policy.score_weights",
            ..
        })
    ));

    let mut unsupported_snapshot = snapshot("unsupported", "codex", 8_000, 900, 1_000, 2_000)?;
    unsupported_snapshot.schema_version = 2;
    assert!(matches!(
        decide_route(
            &policy(
                AdaptiveEvaluationModeV1::Enforce,
                UnknownUsagePolicyV1::StaticFallback,
                500
            )?,
            &input(
                vec![candidate("unsupported", "codex", "codex", 5_000)?],
                vec![unsupported_snapshot]
            )?
        ),
        Err(AdaptiveRoutingError::UnsupportedSchemaVersion { .. })
    ));

    let mut bad_input = routing_input;
    bad_input.schema_version = 2;
    assert!(matches!(
        decide_route(
            &policy(
                AdaptiveEvaluationModeV1::Enforce,
                UnknownUsagePolicyV1::StaticFallback,
                500
            )?,
            &bad_input
        ),
        Err(AdaptiveRoutingError::UnsupportedSchemaVersion { .. })
    ));

    let record: RoutingReplayRecordV1 =
        serde_json::from_str(include_str!("fixtures/adaptive-routing-replay-v1.json"))?;
    let mut tampered_decision = record.expected_decision().clone();
    if let RouteOutcomeV1::Route { route } = &mut tampered_decision.dispatch_outcome {
        route.model = "tampered-model".to_owned();
    }
    assert_eq!(
        RoutingReplayRecordV1::new(
            record.policy().clone(),
            record.input().clone(),
            tampered_decision,
        ),
        Err(AdaptiveRoutingError::ReplayMismatch)
    );

    let mut invalid_fixed_mode = record.expected_decision().clone();
    invalid_fixed_mode.decision_mode = RoutingDecisionModeV1::Adaptive {
        evaluation_mode: AdaptiveEvaluationModeV1::Shadow,
    };
    assert!(matches!(
        RoutingReplayRecordV1::new(
            record.policy().clone(),
            record.input().clone(),
            invalid_fixed_mode,
        ),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "routing_decision.decision_mode",
            ..
        })
    ));

    let reserve_order_record: RoutingReplayRecordV1 = serde_json::from_str(include_str!(
        "fixtures/adaptive-routing-adaptive-replay-v1.json"
    ))?;
    let mut invalid_reserve_order = reserve_order_record.expected_decision().clone();
    if let Some(applied_policy) = &mut invalid_reserve_order.applied_policy {
        applied_policy.provider_reserves.reverse();
    }
    assert!(matches!(
        RoutingReplayRecordV1::new(
            reserve_order_record.policy().clone(),
            reserve_order_record.input().clone(),
            invalid_reserve_order,
        ),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "routing_decision.applied_policy.provider_reserves",
            ..
        })
    ));
    Ok(())
}

#[test]
fn collection_bounds_and_incumbent_references_fail_closed() -> TestResult {
    let routing_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::StaticFallback,
        500,
    )?;

    let mut too_many_candidates = Vec::new();
    for index in 0..129 {
        too_many_candidates.push(candidate(
            &format!("candidate-{index}"),
            "codex",
            "codex",
            5_000,
        )?);
    }
    assert!(matches!(
        decide_route(&routing_policy, &input(too_many_candidates, Vec::new())?),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "routing_input.candidates",
            ..
        })
    ));

    let mut too_many_snapshots = Vec::new();
    for index in 0..257 {
        too_many_snapshots.push(snapshot(
            &format!("snapshot-{index}"),
            "codex",
            8_000,
            900,
            1_000,
            2_000,
        )?);
    }
    assert!(matches!(
        decide_route(&routing_policy, &input(Vec::new(), too_many_snapshots)?),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "routing_input.usage_snapshots",
            ..
        })
    ));

    let mut window_bound = snapshot("window-bound", "codex", 8_000, 900, 1_000, 2_000)?;
    let window = window_bound.windows[0].clone();
    window_bound.windows = vec![window; 33];
    assert!(matches!(
        decide_route(
            &routing_policy,
            &input(
                vec![candidate("window-candidate", "codex", "codex", 5_000)?],
                vec![window_bound]
            )?
        ),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "usage_snapshot.windows",
            ..
        })
    ));

    let mut model_bound = snapshot("model-bound", "codex", 8_000, 900, 1_000, 2_000)?;
    for index in 0..129 {
        model_bound.windows[0]
            .applicable_models
            .insert(format!("model-{index}"));
    }
    assert!(matches!(
        decide_route(
            &routing_policy,
            &input(
                vec![candidate("model-candidate", "codex", "codex", 5_000)?],
                vec![model_bound]
            )?
        ),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "usage_window.applicable_models",
            ..
        })
    ));

    let mut capability_bound = input(
        vec![candidate("capability-candidate", "codex", "codex", 5_000)?],
        Vec::new(),
    )?;
    for index in 0..65 {
        capability_bound
            .requirements
            .required_capabilities
            .insert(CapabilityIdV1::new(format!("required-{index}"))?);
    }
    assert!(matches!(
        decide_route(&routing_policy, &capability_bound),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "routing_input.required_capabilities",
            ..
        })
    ));

    let mut candidate_capability_bound = input(
        vec![candidate("candidate-cap-bound", "codex", "codex", 5_000)?],
        Vec::new(),
    )?;
    for index in 0..65 {
        candidate_capability_bound.candidates[0]
            .capabilities
            .insert(CapabilityIdV1::new(format!("capability-{index}"))?);
    }
    assert!(matches!(
        decide_route(&routing_policy, &candidate_capability_bound),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "adaptive_candidate.capabilities",
            ..
        })
    ));

    let mut reserve_bound = routing_policy.clone();
    reserve_bound.provider_reserves.clear();
    for index in 0..65 {
        reserve_bound.provider_reserves.push(ProviderReserveV1 {
            provider: ProviderIdV1::new(format!("provider-{index}"))?,
            usage_account_id: UsageAccountIdV1::new("primary")?,
            reserve_bps: bp(1_000)?,
        });
    }
    assert!(matches!(
        decide_route(&reserve_bound, &input(Vec::new(), Vec::new())?),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "adaptive_policy.provider_reserves",
            ..
        })
    ));

    let mut missing_incumbent = input(
        vec![candidate("configured", "codex", "codex", 5_000)?],
        Vec::new(),
    )?;
    missing_incumbent.incumbent_candidate_id = Some(CandidateIdV1::new("missing")?);
    assert!(matches!(
        decide_route(&routing_policy, &missing_incumbent),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "routing_input.incumbent_candidate_id",
            ..
        })
    ));

    let primary_route = candidate("primary-route", "codex", "codex", 5_000)?;
    let mut aliased_route = primary_route.clone();
    aliased_route.route.candidate_id = CandidateIdV1::new("aliased-route")?;
    assert_eq!(
        decide_route(
            &routing_policy,
            &input(vec![primary_route, aliased_route], Vec::new())?
        ),
        Err(AdaptiveRoutingError::DuplicateExecutableRoute)
    );

    let mut empty_fixed_provenance = input(Vec::new(), Vec::new())?;
    empty_fixed_provenance.authority.resolved_fixed_route = Some(FixedRouteV1 {
        composition_version: ADAPTIVE_ROUTING_SCHEMA_V1,
        provenances: BTreeSet::new(),
        route: route("fixed", "codex", "codex")?,
    });
    assert!(matches!(
        decide_route(&routing_policy, &empty_fixed_provenance),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "fixed_route.provenances",
            ..
        })
    ));
    Ok(())
}

#[test]
fn wire_shape_omits_account_labels_raw_payloads_and_session_ids() -> TestResult {
    let usage = snapshot("private", "codex", 9_000, 900, 1_000, 2_000)?;
    let routing_input = input(
        vec![candidate("codex", "codex", "codex", 9_000)?],
        vec![usage],
    )?;
    let wire = serde_json::to_string(&routing_input)?;
    assert!(!wire.contains("account_label"));
    assert!(!wire.contains("raw_payload"));
    assert!(!wire.contains("session_id"));
    Ok(())
}

#[test]
fn equal_time_conflicting_snapshots_defer_without_alias_fallback() -> TestResult {
    let route_candidate = candidate("conflicted", "codex", "codex", 8_000)?;
    let first = snapshot("conflict-a", "codex", 9_000, 900, 1_000, 2_000)?;
    let second = snapshot("conflict-b", "codex", 8_000, 900, 1_000, 2_000)?;
    let mut routing_input = input(
        vec![route_candidate.clone()],
        vec![second.clone(), first.clone()],
    )?;
    routing_input.authority.legacy_route = RouteTargetV1 {
        candidate_id: CandidateIdV1::new("conflicted-alias")?,
        ..route_candidate.route.clone()
    };
    let routing_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::StaticFallback,
        500,
    )?;

    let decision = decide_route(&routing_policy, &routing_input)?;
    let conflicted = evaluation(&decision, "conflicted")
        .ok_or_else(|| std::io::Error::other("missing conflicted evaluation"))?;
    assert_eq!(
        conflicted.disposition,
        CandidateDispositionV1::Rejected {
            reason: CandidateRejectionV1::ConflictingUsageSnapshots
        }
    );
    assert_eq!(
        conflicted.governing_snapshot_ids,
        BTreeSet::from([
            SnapshotIdV1::new("conflict-a")?,
            SnapshotIdV1::new("conflict-b")?,
        ])
    );
    assert_eq!(
        conflicted.considered_snapshot_ids,
        conflicted.governing_snapshot_ids
    );
    assert_eq!(
        decision.dispatch_outcome,
        RouteOutcomeV1::Deferred {
            reason: RouteDeferralReasonV1::ConflictingUsageEvidence
        }
    );
    assert_eq!(decision.continuation, ContinuationDispositionV1::NoDispatch);
    assert_eq!(decision.referenced_snapshots, vec![first, second]);
    assert_eq!(decision.applied_policy, Some(routing_policy.clone()));
    assert_eq!(decision.requirements, routing_input.requirements);
    assert_eq!(
        decision.legacy_route.as_ref(),
        Some(&routing_input.authority.legacy_route)
    );

    routing_input.usage_snapshots.reverse();
    assert_eq!(decide_route(&routing_policy, &routing_input)?, decision);
    Ok(())
}

#[test]
fn duration_milliseconds_preserve_nonminute_windows_and_enforce_v1_bound() -> TestResult {
    let mut usage = snapshot("ninety-second", "codex", 9_000, 900, 1_000, 90_900)?;
    usage.windows[0].duration_ms = DurationMillisV1::new(90_000)?;
    let routing_input = input(
        vec![candidate("nonminute", "codex", "codex", 8_000)?],
        vec![usage],
    )?;
    let decision = decide_route(
        &policy(
            AdaptiveEvaluationModeV1::Enforce,
            UnknownUsagePolicyV1::StaticFallback,
            500,
        )?,
        &routing_input,
    )?;
    assert_eq!(
        dispatch_route(&decision).map(|route| route.candidate_id.as_str()),
        Some("nonminute")
    );
    let wire = serde_json::to_value(&routing_input)?;
    assert_eq!(
        wire.pointer("/usage_snapshots/0/windows/0/duration_ms"),
        Some(&serde_json::json!(90_000))
    );
    assert!(
        wire.pointer("/usage_snapshots/0/windows/0/duration_minutes")
            .is_none()
    );
    assert_eq!(
        DurationMillisV1::new(ADAPTIVE_MAX_DURATION_MS_V1 + 1),
        Err(AdaptiveRoutingError::DurationOutOfRange {
            found: ADAPTIVE_MAX_DURATION_MS_V1 + 1,
            maximum: ADAPTIVE_MAX_DURATION_MS_V1,
        })
    );
    Ok(())
}

#[test]
fn shadow_mode_dispatches_legacy_when_recommendation_is_deferred() -> TestResult {
    let mut forbidden = candidate("shadow-forbidden", "codex", "codex", 8_000)?;
    forbidden.automatic_eligibility = AutomaticEligibilityV1::ExplicitOnly;
    let mut routing_input = input(vec![forbidden], Vec::new())?;
    routing_input.existing_session_runtime = Some(RuntimeIdV1::new("claude")?);

    let decision = decide_route(
        &policy(
            AdaptiveEvaluationModeV1::Shadow,
            UnknownUsagePolicyV1::StaticFallback,
            500,
        )?,
        &routing_input,
    )?;
    assert_eq!(
        decision.recommended_outcome,
        RouteOutcomeV1::Deferred {
            reason: RouteDeferralReasonV1::HardConstraint
        }
    );
    assert_eq!(
        dispatch_route(&decision).map(|route| route.candidate_id.as_str()),
        Some("legacy")
    );
    assert_eq!(
        decision.continuation,
        ContinuationDispositionV1::SameRuntimeMayResume
    );
    Ok(())
}

#[test]
fn hold_incumbent_never_overrides_hard_or_capacity_denials() -> TestResult {
    let mut hard_candidate = candidate("hard-incumbent", "codex", "codex", 8_000)?;
    hard_candidate.automatic_eligibility = AutomaticEligibilityV1::ExplicitOnly;
    let mut hard_input = input(vec![hard_candidate], Vec::new())?;
    hard_input.incumbent_candidate_id = Some(CandidateIdV1::new("hard-incumbent")?);
    let hold_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::HoldIncumbent,
        500,
    )?;
    let hard = decide_route(&hold_policy, &hard_input)?;
    assert_eq!(
        hard.dispatch_outcome,
        RouteOutcomeV1::Deferred {
            reason: RouteDeferralReasonV1::HardConstraint
        }
    );

    let mut capacity_input = input(
        vec![candidate("capacity-incumbent", "codex", "codex", 8_000)?],
        vec![snapshot(
            "capacity-exhausted",
            "codex",
            0,
            900,
            1_000,
            2_000,
        )?],
    )?;
    capacity_input.incumbent_candidate_id = Some(CandidateIdV1::new("capacity-incumbent")?);
    let capacity = decide_route(&hold_policy, &capacity_input)?;
    assert_eq!(
        capacity.dispatch_outcome,
        RouteOutcomeV1::Deferred {
            reason: RouteDeferralReasonV1::CapacityUnavailable
        }
    );
    assert_eq!(hard.continuation, ContinuationDispositionV1::NoDispatch);
    assert_eq!(capacity.continuation, ContinuationDispositionV1::NoDispatch);
    Ok(())
}

#[test]
fn defer_policy_never_routes_around_any_adverse_usage_evidence() -> TestResult {
    let missing = input(
        vec![candidate("defer-missing", "codex", "codex", 8_000)?],
        Vec::new(),
    )?;
    let stale = input(
        vec![candidate("defer-stale", "codex", "codex", 8_000)?],
        vec![snapshot(
            "defer-stale-usage",
            "codex",
            9_000,
            0,
            1_000,
            2_000,
        )?],
    )?;
    let clock_skew = input(
        vec![candidate("defer-skew", "codex", "codex", 8_000)?],
        vec![snapshot(
            "defer-skew-usage",
            "codex",
            9_000,
            1_100,
            1_000,
            2_000,
        )?],
    )?;
    let reset_elapsed = input(
        vec![candidate("defer-reset", "codex", "codex", 8_000)?],
        vec![snapshot(
            "defer-reset-usage",
            "codex",
            9_000,
            900,
            1_000,
            1_000,
        )?],
    )?;
    let mut unavailable_snapshot =
        snapshot("defer-unavailable-usage", "codex", 9_000, 900, 1_000, 2_000)?;
    unavailable_snapshot.health = UsageHealthV1::Unavailable;
    unavailable_snapshot.confidence_bps = bp(0)?;
    unavailable_snapshot.reason_code = Some(UsageReasonCodeV1::new("adapter_unavailable")?);
    unavailable_snapshot.windows.clear();
    let unavailable = input(
        vec![candidate("defer-unavailable", "codex", "codex", 8_000)?],
        vec![unavailable_snapshot],
    )?;
    let mut model_scoped_snapshot =
        snapshot("defer-model-scope-usage", "codex", 9_000, 900, 1_000, 2_000)?;
    model_scoped_snapshot.windows[0].applicable_models =
        BTreeSet::from(["different-model".to_owned()]);
    let no_window = input(
        vec![candidate("defer-model-scope", "codex", "codex", 8_000)?],
        vec![model_scoped_snapshot],
    )?;
    let conflict = input(
        vec![candidate("defer-conflict", "codex", "codex", 8_000)?],
        vec![
            snapshot("defer-conflict-a", "codex", 9_000, 900, 1_000, 2_000)?,
            snapshot("defer-conflict-b", "codex", 8_000, 900, 1_000, 2_000)?,
        ],
    )?;
    let exhausted = input(
        vec![candidate("defer-exhausted", "codex", "codex", 8_000)?],
        vec![snapshot(
            "defer-exhausted-usage",
            "codex",
            0,
            900,
            1_000,
            2_000,
        )?],
    )?;
    let reserve_protected = input(
        vec![candidate("defer-reserve", "codex", "codex", 8_000)?],
        vec![snapshot(
            "defer-reserve-usage",
            "codex",
            1_000,
            900,
            1_000,
            2_000,
        )?],
    )?;
    let mut explicit_candidate = candidate("defer-explicit", "codex", "codex", 8_000)?;
    explicit_candidate.automatic_eligibility = AutomaticEligibilityV1::ExplicitOnly;
    let explicit = input(vec![explicit_candidate], Vec::new())?;

    let cases = [
        (
            CandidateRejectionV1::MissingUsageSnapshot,
            RouteDeferralReasonV1::UnknownUsage,
            missing,
        ),
        (
            CandidateRejectionV1::StaleUsageSnapshot,
            RouteDeferralReasonV1::UnknownUsage,
            stale,
        ),
        (
            CandidateRejectionV1::UsageClockSkew,
            RouteDeferralReasonV1::UnknownUsage,
            clock_skew,
        ),
        (
            CandidateRejectionV1::ResetElapsed,
            RouteDeferralReasonV1::UnknownUsage,
            reset_elapsed,
        ),
        (
            CandidateRejectionV1::UnhealthyUsage,
            RouteDeferralReasonV1::UnknownUsage,
            unavailable,
        ),
        (
            CandidateRejectionV1::NoApplicableWindow,
            RouteDeferralReasonV1::UnknownUsage,
            no_window,
        ),
        (
            CandidateRejectionV1::ConflictingUsageSnapshots,
            RouteDeferralReasonV1::ConflictingUsageEvidence,
            conflict,
        ),
        (
            CandidateRejectionV1::Exhausted,
            RouteDeferralReasonV1::CapacityUnavailable,
            exhausted,
        ),
        (
            CandidateRejectionV1::ReserveProtected,
            RouteDeferralReasonV1::CapacityUnavailable,
            reserve_protected,
        ),
        (
            CandidateRejectionV1::ExplicitOnly,
            RouteDeferralReasonV1::HardConstraint,
            explicit,
        ),
    ];
    let defer_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::Defer,
        500,
    )?;
    for (rejection, deferral, mut routing_input) in cases {
        let candidate_route = routing_input
            .candidates
            .first()
            .map(|candidate| candidate.route.clone())
            .ok_or_else(|| std::io::Error::other("missing defer candidate"))?;
        routing_input.authority.legacy_route = RouteTargetV1 {
            candidate_id: CandidateIdV1::new(format!(
                "legacy-{}",
                candidate_route.candidate_id.as_str()
            ))?,
            ..candidate_route
        };
        let decision = decide_route(&defer_policy, &routing_input)?;
        assert_eq!(
            decision
                .candidate_evaluations
                .first()
                .map(|evaluation| evaluation.disposition),
            Some(CandidateDispositionV1::Rejected { reason: rejection })
        );
        assert_eq!(
            decision.dispatch_outcome,
            RouteOutcomeV1::Deferred { reason: deferral }
        );
        assert_eq!(decision.switch_reason, SwitchReasonV1::NoEligibleCandidate);
        assert_eq!(decision.continuation, ContinuationDispositionV1::NoDispatch);
    }

    let empty = decide_route(&defer_policy, &input(Vec::new(), Vec::new())?)?;
    assert_eq!(
        empty.dispatch_outcome,
        RouteOutcomeV1::Deferred {
            reason: RouteDeferralReasonV1::NoAdaptiveCandidate
        }
    );

    let mut mixed_hard = candidate("defer-mixed-hard", "codex", "codex", 8_000)?;
    mixed_hard.automatic_eligibility = AutomaticEligibilityV1::ExplicitOnly;
    let mixed = decide_route(
        &defer_policy,
        &input(
            vec![
                mixed_hard,
                candidate("defer-mixed-unknown", "codex", "codex", 8_000)?,
            ],
            Vec::new(),
        )?,
    )?;
    assert_eq!(
        mixed.dispatch_outcome,
        RouteOutcomeV1::Deferred {
            reason: RouteDeferralReasonV1::MixedIneligibility
        }
    );
    Ok(())
}

#[test]
fn older_usable_snapshot_survives_newer_unusable_observation() -> TestResult {
    let older = snapshot("older-usable", "codex", 9_000, 900, 1_000, 2_000)?;
    let mut newer = snapshot("newer-unusable", "codex", 8_000, 950, 1_000, 2_000)?;
    newer.health = UsageHealthV1::Unavailable;
    newer.confidence_bps = bp(0)?;
    newer.reason_code = Some(UsageReasonCodeV1::new("adapter_unavailable")?);
    newer.windows.clear();
    let routing_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::StaticFallback,
        500,
    )?;
    let mut routing_input = input(
        vec![candidate("cached", "codex", "codex", 8_000)?],
        vec![newer.clone(), older.clone()],
    )?;

    let decision = decide_route(&routing_policy, &routing_input)?;
    let cached = evaluation(&decision, "cached")
        .ok_or_else(|| std::io::Error::other("missing cached evaluation"))?;
    assert_eq!(
        cached.considered_snapshot_ids,
        BTreeSet::from([
            SnapshotIdV1::new("newer-unusable")?,
            SnapshotIdV1::new("older-usable")?,
        ])
    );
    assert_eq!(
        cached.governing_snapshot_ids,
        BTreeSet::from([SnapshotIdV1::new("older-usable")?])
    );
    assert_eq!(cached.reported_remaining_bps, Some(bp(9_000)?));
    assert_eq!(decision.referenced_snapshots, vec![newer, older]);

    routing_input.usage_snapshots.reverse();
    assert_eq!(decide_route(&routing_policy, &routing_input)?, decision);
    Ok(())
}

#[test]
fn duplicate_provenance_and_malicious_usage_identifiers_fail_deserialization() -> TestResult {
    let mut fixed_input = input(Vec::new(), Vec::new())?;
    fixed_input.authority.resolved_fixed_route = Some(FixedRouteV1 {
        composition_version: ADAPTIVE_ROUTING_SCHEMA_V1,
        provenances: BTreeSet::from([FixedAuthorityProvenanceV1::ModelFlag]),
        route: route("fixed-duplicate", "codex", "codex")?,
    });
    let mut input_wire = serde_json::to_value(&fixed_input)?;
    let provenances = input_wire
        .pointer_mut("/authority/resolved_fixed_route/provenances")
        .ok_or_else(|| std::io::Error::other("missing fixed provenance wire field"))?;
    *provenances = serde_json::json!(["model_flag", "model_flag"]);
    assert!(serde_json::from_value::<RoutingInputV1>(input_wire).is_err());

    let fixed_decision = decide_route(
        &policy(
            AdaptiveEvaluationModeV1::Enforce,
            UnknownUsagePolicyV1::StaticFallback,
            500,
        )?,
        &fixed_input,
    )?;
    let mut decision_wire = serde_json::to_value(&fixed_decision)?;
    let provenances = decision_wire
        .pointer_mut("/applied_authority/provenances")
        .ok_or_else(|| std::io::Error::other("missing decision provenance wire field"))?;
    *provenances = serde_json::json!(["model_flag", "model_flag"]);
    assert!(serde_json::from_value::<RoutingDecisionV1>(decision_wire).is_err());

    assert!(UsageSourceIdV1::new("adapter@example.com").is_err());
    assert!(UsageSourceIdV1::new("adapter\u{202e}txt").is_err());
    assert!(UsageWindowKindV1::new("rolling/window").is_err());
    let valid_snapshot_wire = serde_json::to_value(snapshot(
        "identifier-wire",
        "codex",
        9_000,
        900,
        1_000,
        2_000,
    )?)?;
    let mut snapshot_wire = valid_snapshot_wire.clone();
    *snapshot_wire
        .pointer_mut("/source")
        .ok_or_else(|| std::io::Error::other("missing source wire field"))? =
        serde_json::json!("adapter\u{202e}txt");
    assert!(serde_json::from_value::<UsageSnapshotV1>(snapshot_wire).is_err());
    let mut kind_wire = valid_snapshot_wire;
    *kind_wire
        .pointer_mut("/windows/0/kind")
        .ok_or_else(|| std::io::Error::other("missing window kind wire field"))? =
        serde_json::json!("rolling/window");
    assert!(serde_json::from_value::<UsageSnapshotV1>(kind_wire).is_err());
    Ok(())
}

#[test]
fn persisted_route_strings_accept_only_printable_ascii_without_edge_whitespace() -> TestResult {
    let routing_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::Defer,
        500,
    )?;
    for invalid_model in [
        " leading",
        "trailing ",
        "bidi\u{202e}model",
        "zero\u{200b}width",
        "soft\u{00ad}hyphen",
        "variant\u{fe0f}",
        "variation\u{180b}",
        "variation\u{180d}",
        "reserved\u{2065}",
        "visible-café",
        "visible-模型",
        "line\nbreak",
    ] {
        let mut routing_input = input(Vec::new(), Vec::new())?;
        routing_input.authority.resolved_fixed_route = Some(FixedRouteV1 {
            composition_version: ADAPTIVE_ROUTING_SCHEMA_V1,
            provenances: BTreeSet::from([FixedAuthorityProvenanceV1::ModelFlag]),
            route: RouteTargetV1 {
                model: invalid_model.to_owned(),
                ..route("fixed-invalid-model", "codex", "codex")?
            },
        });
        assert!(matches!(
            decide_route(&routing_policy, &routing_input),
            Err(AdaptiveRoutingError::InvalidText {
                field: "route.model",
                ..
            })
        ));
    }

    for invalid_effort in [" high", "high ", "hi\u{2066}gh", "très-high"] {
        let mut routing_input = input(Vec::new(), Vec::new())?;
        routing_input.authority.resolved_fixed_route = Some(FixedRouteV1 {
            composition_version: ADAPTIVE_ROUTING_SCHEMA_V1,
            provenances: BTreeSet::from([FixedAuthorityProvenanceV1::ModelFlag]),
            route: RouteTargetV1 {
                effort: Some(invalid_effort.to_owned()),
                ..route("fixed-invalid-effort", "codex", "codex")?
            },
        });
        assert!(matches!(
            decide_route(&routing_policy, &routing_input),
            Err(AdaptiveRoutingError::InvalidText {
                field: "route.effort",
                ..
            })
        ));
    }

    for invalid_applicable_model in [
        " model",
        "model ",
        "model\u{202e}",
        "model\u{034f}",
        "model\u{180b}",
        "model\u{180d}",
        "model\u{2065}",
        "modèle",
    ] {
        let route_candidate = candidate("applicable-model", "codex", "codex", 8_000)?;
        let mut usage = snapshot("applicable-model-usage", "codex", 9_000, 900, 1_000, 2_000)?;
        usage.windows[0].applicable_models = BTreeSet::from([invalid_applicable_model.to_owned()]);
        assert!(matches!(
            decide_route(&routing_policy, &input(vec![route_candidate], vec![usage])?),
            Err(AdaptiveRoutingError::InvalidText {
                field: "usage_window.applicable_model",
                ..
            })
        ));
    }

    let mut printable_input = input(Vec::new(), Vec::new())?;
    printable_input.authority.resolved_fixed_route = Some(FixedRouteV1 {
        composition_version: ADAPTIVE_ROUTING_SCHEMA_V1,
        provenances: BTreeSet::from([FixedAuthorityProvenanceV1::ModelFlag]),
        route: RouteTargetV1 {
            model: "!model name~".to_owned(),
            effort: Some("very high~".to_owned()),
            ..route("fixed-printable-ascii", "codex", "codex")?
        },
    });
    let printable = decide_route(&routing_policy, &printable_input)?;
    assert_eq!(
        dispatch_route(&printable).map(|route| (route.model.as_str(), route.effort.as_deref())),
        Some(("!model name~", Some("very high~")))
    );
    Ok(())
}

#[test]
fn aggregate_element_text_and_evaluation_budgets_fail_closed() -> TestResult {
    let routing_policy = policy(
        AdaptiveEvaluationModeV1::Enforce,
        UnknownUsagePolicyV1::StaticFallback,
        500,
    )?;

    let mut element_candidates = Vec::new();
    for candidate_index in 0..128 {
        let mut route_candidate = candidate(
            &format!("element-{candidate_index}"),
            "codex",
            "codex",
            5_000,
        )?;
        for capability_index in 0..63 {
            route_candidate
                .capabilities
                .insert(CapabilityIdV1::new(format!(
                    "cap-{candidate_index}-{capability_index}"
                ))?);
        }
        element_candidates.push(route_candidate);
    }
    assert!(matches!(
        decide_route(&routing_policy, &input(element_candidates, Vec::new())?),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "adaptive_routing.aggregate_elements",
            ..
        })
    ));

    let short_models = (0..128)
        .map(|index| format!("model-{index}"))
        .collect::<BTreeSet<_>>();
    let mut model_reference_snapshots = Vec::new();
    for snapshot_index in 0..33 {
        let mut usage = snapshot(
            &format!("model-refs-{snapshot_index}"),
            "codex",
            9_000,
            900,
            1_000,
            2_000,
        )?;
        usage.windows[0].applicable_models = short_models.clone();
        model_reference_snapshots.push(usage);
    }
    assert!(matches!(
        decide_route(
            &routing_policy,
            &input(Vec::new(), model_reference_snapshots)?
        ),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "adaptive_routing.total_model_references",
            ..
        })
    ));

    let mut window_snapshots = Vec::new();
    for snapshot_index in 0..17 {
        let mut usage = snapshot(
            &format!("window-total-{snapshot_index}"),
            "codex",
            9_000,
            900,
            1_000,
            2_000,
        )?;
        let template = usage.windows[0].clone();
        for window_index in 1..32 {
            let mut window = template.clone();
            window.limit_id = UsageLimitIdV1::new(format!("limit-{window_index}"))?;
            usage.windows.push(window);
        }
        window_snapshots.push(usage);
    }
    assert!(matches!(
        decide_route(&routing_policy, &input(Vec::new(), window_snapshots)?),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "adaptive_routing.total_windows",
            ..
        })
    ));

    let long_models = (0..128)
        .map(|index| {
            let prefix = format!("{index:03}-");
            format!("{prefix}{}", "x".repeat(256 - prefix.len()))
        })
        .collect::<BTreeSet<_>>();
    let mut text_snapshots = Vec::new();
    for snapshot_index in 0..32 {
        let mut usage = snapshot(
            &format!("text-bytes-{snapshot_index}"),
            "codex",
            9_000,
            900,
            1_000,
            2_000,
        )?;
        usage.windows[0].applicable_models = long_models.clone();
        text_snapshots.push(usage);
    }
    assert!(matches!(
        decide_route(&routing_policy, &input(Vec::new(), text_snapshots)?),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "adaptive_routing.aggregate_text_bytes",
            ..
        })
    ));

    let mut evaluation_candidates = Vec::new();
    for index in 0..128 {
        evaluation_candidates.push(candidate(
            &format!("evaluation-{index}"),
            "codex",
            "codex",
            5_000,
        )?);
    }
    let mut evaluation_snapshots = Vec::new();
    for index in 0..65 {
        evaluation_snapshots.push(snapshot(
            &format!("evaluation-usage-{index}"),
            "codex",
            9_000,
            900,
            1_000,
            2_000,
        )?);
    }
    assert!(matches!(
        decide_route(
            &routing_policy,
            &input(evaluation_candidates.clone(), evaluation_snapshots)?
        ),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "adaptive_routing.considered_snapshot_references",
            ..
        })
    ));

    let mut evaluation_work_snapshots = Vec::new();
    for snapshot_index in 0..9 {
        let mut usage = snapshot(
            &format!("evaluation-work-{snapshot_index}"),
            "codex",
            9_000,
            900,
            1_000,
            2_000,
        )?;
        let template = usage.windows[0].clone();
        for window_index in 1..32 {
            let mut window = template.clone();
            window.limit_id = UsageLimitIdV1::new(format!("work-limit-{window_index}"))?;
            usage.windows.push(window);
        }
        evaluation_work_snapshots.push(usage);
    }
    assert!(matches!(
        decide_route(
            &routing_policy,
            &input(evaluation_candidates, evaluation_work_snapshots)?
        ),
        Err(AdaptiveRoutingError::InvalidInvariant {
            field: "adaptive_routing.usage_window_evaluations",
            ..
        })
    ));
    Ok(())
}
