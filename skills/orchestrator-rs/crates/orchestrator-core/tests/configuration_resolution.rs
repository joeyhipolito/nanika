use orchestrator_core::{
    AdvisorConfig, ConfigError, ModelResolutionInput, RoutingMap, RoutingTier,
    RuntimeResolutionInput, RuntimeSource, StallResolutionInput, StallSource, StallTimeoutValue,
    resolve_advisor_config, resolve_model, resolve_runtime, resolve_stall_timeout,
};
use std::{collections::BTreeMap, time::Duration};

#[test]
fn runtime_precedence_and_validation_are_table_driven() {
    let base = RuntimeResolutionInput {
        authored_runtime: Some("codex".to_owned()),
        runtime_policy_applied: false,
        forced_runtime: Some("both".to_owned()),
        environment_runtime: Some("openai-api".to_owned()),
        configured_tier_runtime: Some("openrouter".to_owned()),
        policy_runtime: Some("gemini-api".to_owned()),
    };
    let cases = [
        (base.clone(), "codex", RuntimeSource::Authored, false),
        (
            RuntimeResolutionInput {
                authored_runtime: None,
                runtime_policy_applied: true,
                ..base.clone()
            },
            "both",
            RuntimeSource::Forced,
            false,
        ),
        (
            RuntimeResolutionInput {
                authored_runtime: None,
                runtime_policy_applied: true,
                forced_runtime: None,
                ..base.clone()
            },
            "openai-api",
            RuntimeSource::Environment,
            false,
        ),
        (
            RuntimeResolutionInput {
                authored_runtime: None,
                runtime_policy_applied: true,
                forced_runtime: None,
                environment_runtime: None,
                ..base.clone()
            },
            "openrouter",
            RuntimeSource::ConfiguredTier,
            false,
        ),
        (
            RuntimeResolutionInput {
                authored_runtime: None,
                runtime_policy_applied: true,
                forced_runtime: None,
                environment_runtime: None,
                configured_tier_runtime: None,
                ..base.clone()
            },
            "gemini-api",
            RuntimeSource::Policy,
            false,
        ),
        (
            RuntimeResolutionInput {
                authored_runtime: None,
                runtime_policy_applied: true,
                forced_runtime: None,
                environment_runtime: None,
                configured_tier_runtime: None,
                policy_runtime: None,
            },
            "claude",
            RuntimeSource::EmptyDefault,
            false,
        ),
        (
            RuntimeResolutionInput {
                authored_runtime: None,
                runtime_policy_applied: true,
                forced_runtime: Some("mystery".to_owned()),
                ..base
            },
            "claude",
            RuntimeSource::Forced,
            true,
        ),
        (
            RuntimeResolutionInput {
                authored_runtime: None,
                runtime_policy_applied: true,
                forced_runtime: Some("CODEX".to_owned()),
                environment_runtime: None,
                configured_tier_runtime: None,
                policy_runtime: None,
            },
            "claude",
            RuntimeSource::Forced,
            true,
        ),
    ];
    for (input, runtime, source, unknown) in cases {
        let resolved = resolve_runtime(input);
        assert_eq!(
            (
                resolved.runtime.as_str(),
                resolved.source,
                resolved.unknown_fell_back_to_claude
            ),
            (runtime, source, unknown)
        );
    }
}

#[test]
fn model_map_requires_matching_runtime_and_forced_model_wins() {
    let map = RoutingMap {
        model_tiers: BTreeMap::from([(
            "work".to_owned(),
            RoutingTier {
                provider: "ignored".to_owned(),
                model: "configured".to_owned(),
                runtime: "codex".to_owned(),
            },
        )]),
    };
    let cases = [
        (Some("forced"), "claude", "forced"),
        (None, "codex", "configured"),
        (None, "claude", "built-in"),
    ];
    for (forced, runtime, expected) in cases {
        assert_eq!(
            resolve_model(ModelResolutionInput {
                forced_model: forced.map(str::to_owned),
                tier: "work".to_owned(),
                effective_runtime: runtime.to_owned(),
                routing_map: map.clone(),
                built_in_model: "built-in".to_owned(),
            }),
            expected
        );
    }
}

#[test]
fn model_runtime_matching_is_case_sensitive_like_go() {
    let map = RoutingMap {
        model_tiers: BTreeMap::from([(
            "work".to_owned(),
            RoutingTier {
                provider: String::new(),
                model: "configured".to_owned(),
                runtime: "CODEX".to_owned(),
            },
        )]),
    };
    assert_eq!(
        resolve_model(ModelResolutionInput {
            forced_model: None,
            tier: "work".to_owned(),
            effective_runtime: "codex".to_owned(),
            routing_map: map,
            built_in_model: "built-in".to_owned(),
        }),
        "built-in"
    );
}

#[test]
fn stall_precedence_and_intentional_environment_correction_are_table_driven()
-> Result<(), Box<dyn std::error::Error>> {
    let phase = resolve_stall_timeout(StallResolutionInput {
        phase_timeout: Some(Duration::from_secs(3)),
        flag_value: Some("4s".to_owned()),
        environment_value: Some("5s".to_owned()),
    })?;
    assert_eq!(phase.source, StallSource::Phase);
    assert_eq!(
        phase.value,
        StallTimeoutValue::Duration(Duration::from_secs(3))
    );

    for raw in ["", "garbage", "0s", "-2m"] {
        assert_eq!(
            resolve_stall_timeout(StallResolutionInput {
                phase_timeout: None,
                flag_value: Some(raw.to_owned()),
                environment_value: Some("5s".to_owned())
            }),
            Err(ConfigError::InvalidStallFlag {
                value: raw.to_owned()
            })
        );
        assert_eq!(
            resolve_stall_timeout(StallResolutionInput {
                phase_timeout: None,
                flag_value: None,
                environment_value: Some(raw.to_owned())
            }),
            Err(ConfigError::InvalidStallEnvironment {
                value: raw.to_owned()
            })
        );
    }
    let default = resolve_stall_timeout(StallResolutionInput::default())?;
    assert_eq!(default.source, StallSource::WorkerDefault);
    assert_eq!(default.value, StallTimeoutValue::WorkerDefault);

    let fractional = resolve_stall_timeout(StallResolutionInput {
        phase_timeout: None,
        flag_value: Some("1.5s250ms".to_owned()),
        environment_value: None,
    })?;
    assert_eq!(
        fractional.value,
        StallTimeoutValue::Duration(Duration::from_millis(1_750))
    );

    for raw in ["+1.5s250ms", "+1750ms"] {
        let explicitly_positive = resolve_stall_timeout(StallResolutionInput {
            phase_timeout: None,
            flag_value: Some(raw.to_owned()),
            environment_value: None,
        })?;
        assert_eq!(
            explicitly_positive.value,
            StallTimeoutValue::Duration(Duration::from_millis(1_750)),
            "Go-compatible leading plus was not accepted for {raw}"
        );
    }

    let positive_environment = resolve_stall_timeout(StallResolutionInput {
        phase_timeout: None,
        flag_value: None,
        environment_value: Some("+1750ms".to_owned()),
    })?;
    assert_eq!(positive_environment.source, StallSource::Environment);
    assert_eq!(
        positive_environment.value,
        StallTimeoutValue::Duration(Duration::from_millis(1_750))
    );

    for (raw, expected) in [
        ("0.3333333333333333333h", Duration::from_secs(20 * 60)),
        ("0.100000000000000000000h", Duration::from_secs(6 * 60)),
        (
            "0.830103483285477580700h",
            Duration::from_secs(49 * 60 + 48) + Duration::from_nanos(372_539_827),
        ),
        (
            "9223372036854775807ns",
            Duration::from_nanos(i64::MAX as u64),
        ),
        (
            "9223372036854775.807us",
            Duration::from_nanos(i64::MAX as u64),
        ),
        (
            "9223372036s854ms775us807ns",
            Duration::from_nanos(i64::MAX as u64),
        ),
    ] {
        let resolved = resolve_stall_timeout(StallResolutionInput {
            phase_timeout: None,
            flag_value: Some(raw.to_owned()),
            environment_value: None,
        })?;
        assert_eq!(
            resolved.value,
            StallTimeoutValue::Duration(expected),
            "long fraction diverged from Go time.ParseDuration for {raw}"
        );
    }

    for raw in ["+", "+0s", "++1s", "+-1s"] {
        assert_eq!(
            resolve_stall_timeout(StallResolutionInput {
                phase_timeout: None,
                flag_value: Some(raw.to_owned()),
                environment_value: None,
            }),
            Err(ConfigError::InvalidStallFlag {
                value: raw.to_owned()
            })
        );
    }
    assert!(matches!(
        resolve_stall_timeout(StallResolutionInput {
            phase_timeout: None,
            flag_value: Some("9223372036854775808ns".to_owned()),
            environment_value: None,
        }),
        Err(ConfigError::InvalidStallFlag { .. })
    ));
    assert!(matches!(
        resolve_stall_timeout(StallResolutionInput {
            phase_timeout: None,
            flag_value: Some("9223370836854775808ns0.3333333333333333333h".to_owned()),
            environment_value: None,
        }),
        Err(ConfigError::InvalidStallFlag { .. })
    ));
    Ok(())
}

/// Proves Rust config resolution succeeds — and lands on Go's exact safe
/// defaults — with no `advisorbridge` dependency present anywhere in this
/// crate. Mirrors Go's `LoadAdvisorConfig` contract: missing config.yaml /
/// missing `advisor.unattended_glm` stanza / a malformed stanza all resolve
/// to `DefaultUnattendedConfig()` (`unattended.go:123-144`), i.e. `Enabled:
/// false, EmergencyStop: true` (advisor.go:16-19, "Missing files/stanzas
/// are default-off"; `unattended.go:125`, the fail-safe default).
#[test]
fn advisor_config_resolves_to_safe_default_off_without_advisorbridge() {
    // No config.yaml / no advisor stanza at all: both fields fall back to
    // Go's DefaultUnattendedConfig() values, not a uniform `false`.
    assert_eq!(
        resolve_advisor_config(None, None),
        AdvisorConfig {
            unattended_glm_enabled: false,
            emergency_stop: true,
        }
    );
    // A stanza that was present and parsed clean, both fields explicit.
    assert_eq!(
        resolve_advisor_config(Some(true), Some(false)),
        AdvisorConfig {
            unattended_glm_enabled: true,
            emergency_stop: false,
        }
    );
    assert_eq!(
        resolve_advisor_config(Some(false), Some(true)),
        AdvisorConfig {
            unattended_glm_enabled: false,
            emergency_stop: true,
        }
    );
    // A malformed/absent `emergency_stop` key specifically must not silently
    // flip Go's fail-safe default to `false`.
    assert_eq!(
        resolve_advisor_config(Some(true), None),
        AdvisorConfig {
            unattended_glm_enabled: true,
            emergency_stop: true,
        }
    );
    // `AdvisorConfig::default()` / `default_off()` both land on Go's zero value.
    assert_eq!(AdvisorConfig::default(), AdvisorConfig::default_off());
    assert!(!AdvisorConfig::default_off().unattended_glm_enabled);
    assert!(AdvisorConfig::default_off().emergency_stop);
}
