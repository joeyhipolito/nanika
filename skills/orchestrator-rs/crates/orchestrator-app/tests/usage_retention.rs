use std::collections::{BTreeMap, BTreeSet};

use orchestrator_app::{
    ClaudeStatuslineAdapterPolicyV1, ClaudeStatuslineUsageHandle,
    MAX_RETAINED_OBSERVATIONS_PER_SLOT, MAX_RETAINED_USAGE_SLOTS, PendingDispatch,
    TrustedUsageError, UsageMultiplexer, UsageMultiplexerPolicy,
};
use orchestrator_core::{
    AdaptiveCandidateV1, AdaptiveEvaluationModeV1, AdaptivePolicyV1, AutomaticEligibilityV1,
    BasisPointsV1, CandidateIdV1, DurationMillisV1, MissionId, PhaseId, ProviderIdV1,
    ProviderReserveV1, QualityTierV1, RouteRequirementsV1, RouteTargetV1, RoutingAuthorityV1,
    ScoreWeightsV1, TaskPriorityV1, UnknownUsagePolicyV1, UsageAccountIdV1, UtcMillisV1,
    decide_route,
};
use orchestrator_provider_claude::{
    ClaudeStatuslineQuotaObservationV1, decode_claude_statusline_usage,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

const RESET_AT_SECONDS: u64 = 1_738_425_600;
const RESET_AT_MS: u64 = RESET_AT_SECONDS * 1_000;
const OBSERVED_AT_MS: u64 = RESET_AT_MS - 60_000;

fn adapter_policy() -> Result<ClaudeStatuslineAdapterPolicyV1> {
    Ok(ClaudeStatuslineAdapterPolicyV1::new(
        DurationMillisV1::new(120_000)?,
        BasisPointsV1::new(10_000)?,
        BasisPointsV1::new(9_000)?,
    )?)
}

fn multiplexer_policy() -> Result<UsageMultiplexerPolicy> {
    Ok(UsageMultiplexerPolicy {
        maximum_snapshot_age_ms: DurationMillisV1::new(300_000)?,
        cooldown_ms: DurationMillisV1::new(60_000)?,
    })
}

fn healthy_observation() -> Result<ClaudeStatuslineQuotaObservationV1> {
    let bytes = format!(
        r#"{{
          "version":"2.1.211",
          "rate_limits":{{
            "five_hour":{{"used_percentage":10,"resets_at":{RESET_AT_SECONDS}}},
            "seven_day":{{"used_percentage":20,"resets_at":1738857600}}
          }}
        }}"#
    );
    Ok(decode_claude_statusline_usage(bytes.as_bytes())?)
}

fn empty_observation() -> Result<ClaudeStatuslineQuotaObservationV1> {
    Ok(decode_claude_statusline_usage(br#"{"version":"2.1.211"}"#)?)
}

fn enroll(owner: &mut UsageMultiplexer, account: &str) -> Result<ClaudeStatuslineUsageHandle> {
    Ok(owner.enroll_claude_statusline(
        ProviderIdV1::new("claude")?,
        UsageAccountIdV1::new(account)?,
        adapter_policy()?,
    )?)
}

fn ingest(
    owner: &mut UsageMultiplexer,
    handle: &ClaudeStatuslineUsageHandle,
    observation: &ClaudeStatuslineQuotaObservationV1,
    observed_at_ms: u64,
) -> Result {
    Ok(owner.ingest_claude_statusline(
        handle,
        observation,
        UtcMillisV1::new(observed_at_ms),
        UtcMillisV1::new(observed_at_ms),
    )?)
}

fn route(account: &str) -> Result<RouteTargetV1> {
    Ok(RouteTargetV1 {
        candidate_id: CandidateIdV1::new(format!("claude-{account}"))?,
        provider: ProviderIdV1::new("claude")?,
        usage_account_id: UsageAccountIdV1::new(account)?,
        runtime: orchestrator_core::RuntimeIdV1::new("claude")?,
        model: "claude-sonnet-5".to_owned(),
        effort: None,
    })
}

fn candidate(account: &str) -> Result<AdaptiveCandidateV1> {
    Ok(AdaptiveCandidateV1 {
        route: route(account)?,
        capabilities: BTreeSet::new(),
        automatic_eligibility: AutomaticEligibilityV1::Automatic,
        quality: QualityTierV1::Standard,
        task_fit_bps: BasisPointsV1::new(10_000)?,
        latency_bps: BasisPointsV1::new(10_000)?,
        configured_preference_bps: BasisPointsV1::new(10_000)?,
    })
}

fn pending_dispatch(evaluated_at_ms: u64, account: &str) -> Result<PendingDispatch> {
    Ok(PendingDispatch {
        mission_id: MissionId::new("20260914-public-usage-retention")?,
        phase_id: PhaseId::new("dispatch")?,
        attempt: 1,
        evaluated_at_utc_ms: UtcMillisV1::new(evaluated_at_ms),
        authority: RoutingAuthorityV1 {
            resolved_fixed_route: None,
            legacy_route: route(account)?,
        },
        requirements: RouteRequirementsV1 {
            priority: TaskPriorityV1::P1,
            required_capabilities: BTreeSet::new(),
            minimum_quality: QualityTierV1::Economy,
        },
        candidates: vec![candidate(account)?],
        incumbent_candidate_id: None,
        existing_session_runtime: None,
    })
}

fn assembled(
    owner: &UsageMultiplexer,
    evaluated_at_ms: u64,
) -> Result<orchestrator_core::RoutingInputV1> {
    Ok(owner.assemble_retained_routing_input(
        pending_dispatch(evaluated_at_ms, "acct-primary")?,
        &multiplexer_policy()?,
        &BTreeMap::new(),
    )?)
}

fn adaptive_policy(account: &str) -> Result<AdaptivePolicyV1> {
    Ok(AdaptivePolicyV1 {
        policy_version: 1,
        maximum_snapshot_age_ms: DurationMillisV1::new(300_000)?,
        provider_reserves: vec![ProviderReserveV1 {
            provider: ProviderIdV1::new("claude")?,
            usage_account_id: UsageAccountIdV1::new(account)?,
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
        evaluation_mode: AdaptiveEvaluationModeV1::Enforce,
    })
}

#[test]
fn exact_slot_capacity_is_accepted_and_the_next_slot_preserves_history() -> Result {
    let decoded = healthy_observation()?;
    let mut owner = UsageMultiplexer::new();
    let mut handles = Vec::new();
    for slot in 0..=MAX_RETAINED_USAGE_SLOTS {
        handles.push(enroll(&mut owner, &format!("acct-{slot:03}"))?);
    }
    for handle in handles.iter().take(MAX_RETAINED_USAGE_SLOTS) {
        for offset in 0..MAX_RETAINED_OBSERVATIONS_PER_SLOT {
            ingest(&mut owner, handle, &decoded, OBSERVED_AT_MS + offset as u64)?;
        }
    }

    let before = assembled(
        &owner,
        OBSERVED_AT_MS + MAX_RETAINED_OBSERVATIONS_PER_SLOT as u64,
    )?;
    assert_eq!(
        before.usage_snapshots.len(),
        MAX_RETAINED_USAGE_SLOTS * MAX_RETAINED_OBSERVATIONS_PER_SLOT
    );
    assert_eq!(
        owner.ingest_claude_statusline(
            &handles[MAX_RETAINED_USAGE_SLOTS],
            &decoded,
            UtcMillisV1::new(OBSERVED_AT_MS),
            UtcMillisV1::new(OBSERVED_AT_MS),
        ),
        Err(TrustedUsageError::RetainedUsageSlotLimitReached {
            maximum: MAX_RETAINED_USAGE_SLOTS,
        })
    );
    let after = assembled(
        &owner,
        OBSERVED_AT_MS + MAX_RETAINED_OBSERVATIONS_PER_SLOT as u64,
    )?;
    assert_eq!(after.usage_snapshots, before.usage_snapshots);
    Ok(())
}

#[test]
fn fifth_observation_rotates_only_its_own_oldest_entry() -> Result {
    let decoded = healthy_observation()?;
    let mut owner = UsageMultiplexer::new();
    let handle = enroll(&mut owner, "acct-primary")?;
    for offset in 0..=MAX_RETAINED_OBSERVATIONS_PER_SLOT {
        ingest(
            &mut owner,
            &handle,
            &decoded,
            OBSERVED_AT_MS + offset as u64,
        )?;
    }

    let input = assembled(
        &owner,
        OBSERVED_AT_MS + MAX_RETAINED_OBSERVATIONS_PER_SLOT as u64 + 1,
    )?;
    let observed = input
        .usage_snapshots
        .iter()
        .map(|snapshot| snapshot.observed_at_utc_ms.get())
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        (1..=MAX_RETAINED_OBSERVATIONS_PER_SLOT)
            .map(|offset| OBSERVED_AT_MS + offset as u64)
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn noisy_slot_rotation_preserves_a_quiet_slot() -> Result {
    let decoded = healthy_observation()?;
    let mut owner = UsageMultiplexer::new();
    let noisy = enroll(&mut owner, "acct-noisy")?;
    let quiet = enroll(&mut owner, "acct-quiet")?;
    ingest(&mut owner, &quiet, &decoded, OBSERVED_AT_MS)?;
    for offset in 0..=MAX_RETAINED_OBSERVATIONS_PER_SLOT {
        ingest(&mut owner, &noisy, &decoded, OBSERVED_AT_MS + offset as u64)?;
    }

    let input = assembled(
        &owner,
        OBSERVED_AT_MS + MAX_RETAINED_OBSERVATIONS_PER_SLOT as u64 + 1,
    )?;
    assert_eq!(
        input
            .usage_snapshots
            .iter()
            .filter(|snapshot| snapshot.usage_account_id.as_str() == "acct-quiet")
            .count(),
        1
    );
    assert_eq!(
        input
            .usage_snapshots
            .iter()
            .filter(|snapshot| snapshot.usage_account_id.as_str() == "acct-noisy")
            .count(),
        MAX_RETAINED_OBSERVATIONS_PER_SLOT
    );
    Ok(())
}

#[test]
fn duplicate_stale_and_invalid_admission_preserve_history() -> Result {
    let decoded = healthy_observation()?;
    let mut owner = UsageMultiplexer::new();
    let handle = enroll(&mut owner, "acct-primary")?;
    ingest(&mut owner, &handle, &decoded, OBSERVED_AT_MS)?;
    let before = assembled(&owner, OBSERVED_AT_MS + 1)?;

    for rejected_at in [OBSERVED_AT_MS, OBSERVED_AT_MS - 1] {
        assert_eq!(
            owner.ingest_claude_statusline(
                &handle,
                &decoded,
                UtcMillisV1::new(rejected_at),
                UtcMillisV1::new(rejected_at),
            ),
            Err(TrustedUsageError::NonMonotonicRetainedObservation)
        );
    }
    assert_eq!(
        owner.ingest_claude_statusline(
            &handle,
            &decoded,
            UtcMillisV1::new(OBSERVED_AT_MS + 2),
            UtcMillisV1::new(OBSERVED_AT_MS + 1),
        ),
        Err(TrustedUsageError::ObservationAfterReceipt)
    );
    assert_eq!(
        owner.ingest_claude_statusline(
            &handle,
            &decoded,
            UtcMillisV1::new(RESET_AT_MS),
            UtcMillisV1::new(RESET_AT_MS),
        ),
        Err(TrustedUsageError::InvalidProviderWindowTiming)
    );

    assert_eq!(
        assembled(&owner, OBSERVED_AT_MS + 1)?.usage_snapshots,
        before.usage_snapshots
    );
    Ok(())
}

#[test]
fn foreign_handle_cannot_ingest_or_change_an_owner_history() -> Result {
    let decoded = healthy_observation()?;
    let mut origin = UsageMultiplexer::new();
    let foreign = enroll(&mut origin, "acct-primary")?;
    let mut owner = UsageMultiplexer::new();
    let local = enroll(&mut owner, "acct-primary")?;
    ingest(&mut owner, &local, &decoded, OBSERVED_AT_MS)?;
    let before = assembled(&owner, OBSERVED_AT_MS + 1)?;

    assert_eq!(
        owner.ingest_claude_statusline(
            &foreign,
            &decoded,
            UtcMillisV1::new(OBSERVED_AT_MS + 1),
            UtcMillisV1::new(OBSERVED_AT_MS + 1),
        ),
        Err(TrustedUsageError::InvalidCapability)
    );
    assert_eq!(
        assembled(&owner, OBSERVED_AT_MS + 1)?.usage_snapshots,
        before.usage_snapshots
    );
    Ok(())
}

#[test]
fn interleaved_accounts_assemble_in_exact_slot_then_time_order() -> Result {
    let decoded = healthy_observation()?;
    let mut first = UsageMultiplexer::new();
    let first_a = enroll(&mut first, "acct-a")?;
    let first_b = enroll(&mut first, "acct-b")?;
    for (handle, offset) in [(&first_b, 0), (&first_a, 1), (&first_b, 2), (&first_a, 3)] {
        ingest(&mut first, handle, &decoded, OBSERVED_AT_MS + offset)?;
    }

    let mut second = UsageMultiplexer::new();
    let second_a = enroll(&mut second, "acct-a")?;
    let second_b = enroll(&mut second, "acct-b")?;
    for (handle, offset) in [
        (&second_a, 1),
        (&second_a, 3),
        (&second_b, 0),
        (&second_b, 2),
    ] {
        ingest(&mut second, handle, &decoded, OBSERVED_AT_MS + offset)?;
    }

    let projection = |owner: &UsageMultiplexer| -> Result<Vec<(String, u64)>> {
        Ok(assembled(owner, OBSERVED_AT_MS + 4)?
            .usage_snapshots
            .into_iter()
            .map(|snapshot| {
                (
                    snapshot.usage_account_id.as_str().to_owned(),
                    snapshot.observed_at_utc_ms.get(),
                )
            })
            .collect())
    };
    let expected = vec![
        ("acct-a".to_owned(), OBSERVED_AT_MS + 1),
        ("acct-a".to_owned(), OBSERVED_AT_MS + 3),
        ("acct-b".to_owned(), OBSERVED_AT_MS),
        ("acct-b".to_owned(), OBSERVED_AT_MS + 2),
    ];
    assert_eq!(projection(&first)?, expected);
    assert_eq!(projection(&second)?, expected);
    Ok(())
}

#[test]
fn retained_assembly_skips_history_at_the_caller_floor_and_admits_the_next_boundary() -> Result {
    let decoded = healthy_observation()?;
    let mut owner = UsageMultiplexer::new();
    let handle = enroll(&mut owner, "acct-primary")?;
    ingest(&mut owner, &handle, &decoded, OBSERVED_AT_MS)?;
    let floor = BTreeMap::from([(
        (
            ProviderIdV1::new("claude")?,
            UsageAccountIdV1::new("acct-primary")?,
        ),
        UtcMillisV1::new(OBSERVED_AT_MS),
    )]);
    let policy = UsageMultiplexerPolicy {
        maximum_snapshot_age_ms: DurationMillisV1::new(300_000)?,
        cooldown_ms: DurationMillisV1::new(10_000)?,
    };

    let repeated = owner.assemble_retained_routing_input(
        pending_dispatch(OBSERVED_AT_MS + 1, "acct-primary")?,
        &policy,
        &floor,
    )?;
    assert!(repeated.usage_snapshots.is_empty());

    ingest(&mut owner, &handle, &decoded, OBSERVED_AT_MS + 10_000)?;
    let next = owner.assemble_retained_routing_input(
        pending_dispatch(OBSERVED_AT_MS + 10_001, "acct-primary")?,
        &policy,
        &floor,
    )?;
    assert_eq!(next.usage_snapshots.len(), 1);
    assert_eq!(
        next.usage_snapshots[0].observed_at_utc_ms,
        UtcMillisV1::new(OBSERVED_AT_MS + 10_000)
    );
    Ok(())
}

#[test]
fn retained_assembly_filters_non_evidence_and_feeds_the_pure_router() -> Result {
    let healthy = healthy_observation()?;
    let empty = empty_observation()?;
    let mut owner = UsageMultiplexer::new();
    let expired = enroll(&mut owner, "acct-expired")?;
    let cooling = enroll(&mut owner, "acct-cooling")?;
    let unusable = enroll(&mut owner, "acct-unusable")?;
    ingest(&mut owner, &expired, &healthy, OBSERVED_AT_MS - 120_000)?;
    ingest(&mut owner, &cooling, &healthy, OBSERVED_AT_MS + 100)?;
    ingest(&mut owner, &unusable, &empty, OBSERVED_AT_MS + 200)?;
    let floor = BTreeMap::from([(
        (
            ProviderIdV1::new("claude")?,
            UsageAccountIdV1::new("acct-cooling")?,
        ),
        UtcMillisV1::new(OBSERVED_AT_MS),
    )]);
    let filtered = owner.assemble_retained_routing_input(
        pending_dispatch(OBSERVED_AT_MS + 1_000, "acct-primary")?,
        &multiplexer_policy()?,
        &floor,
    )?;
    assert!(filtered.usage_snapshots.is_empty());

    let primary = enroll(&mut owner, "acct-primary")?;
    ingest(&mut owner, &primary, &healthy, OBSERVED_AT_MS + 300)?;
    let input = owner.assemble_retained_routing_input(
        pending_dispatch(OBSERVED_AT_MS + 1_000, "acct-primary")?,
        &multiplexer_policy()?,
        &floor,
    )?;
    assert_eq!(input.usage_snapshots.len(), 1);
    assert_eq!(
        input.usage_snapshots[0].usage_account_id.as_str(),
        "acct-primary"
    );
    let decision = decide_route(&adaptive_policy("acct-primary")?, &input)?;
    assert_eq!(
        decision
            .dispatch_outcome
            .route()
            .ok_or("expected an executable route")?
            .usage_account_id
            .as_str(),
        "acct-primary"
    );
    Ok(())
}
