use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};
use thiserror::Error;

const SUPPORTED_RUNTIMES: [&str; 7] = [
    "claude",
    "codex",
    "both",
    "anthropic-api",
    "openai-api",
    "openrouter",
    "gemini-api",
];

/// One model-tier entry from the read-only routing configuration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RoutingTier {
    /// Provider is retained for wire compatibility; credential use is outside core.
    #[serde(default)]
    pub provider: String,
    /// Configured model identifier.
    #[serde(default)]
    pub model: String,
    /// Configured runtime identifier.
    #[serde(default)]
    pub runtime: String,
}

/// Deterministic routing values loaded by an application adapter.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RoutingMap {
    /// Entries keyed by tier (`think`, `work`, or `quick`).
    #[serde(default)]
    pub model_tiers: BTreeMap<String, RoutingTier>,
}

/// Inputs to pure runtime precedence resolution.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeResolutionInput {
    pub authored_runtime: Option<String>,
    pub runtime_policy_applied: bool,
    pub forced_runtime: Option<String>,
    pub environment_runtime: Option<String>,
    pub configured_tier_runtime: Option<String>,
    pub policy_runtime: Option<String>,
}

/// The precedence rung which supplied the requested runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeSource {
    Authored,
    Forced,
    Environment,
    ConfiguredTier,
    Policy,
    EmptyDefault,
}

/// Effective runtime and its deterministic provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeResolution {
    pub runtime: String,
    pub source: RuntimeSource,
    /// Set when a selected non-empty value was unsupported and therefore executed as Claude.
    pub unknown_fell_back_to_claude: bool,
}

/// Resolves runtime precedence without reading flags, environment, or files.
#[must_use]
pub fn resolve_runtime(input: RuntimeResolutionInput) -> RuntimeResolution {
    let authored = (!input.runtime_policy_applied)
        .then_some(input.authored_runtime)
        .flatten()
        .and_then(non_empty);
    let policy_applied = input.runtime_policy_applied || authored.is_none();

    let (requested, source) = if let Some(runtime) = authored {
        (runtime, RuntimeSource::Authored)
    } else if policy_applied {
        if let Some(runtime) = input.forced_runtime.and_then(non_empty) {
            (runtime, RuntimeSource::Forced)
        } else if let Some(runtime) = input.environment_runtime.and_then(non_empty) {
            (runtime, RuntimeSource::Environment)
        } else if let Some(runtime) = input.configured_tier_runtime.and_then(non_empty) {
            (runtime, RuntimeSource::ConfiguredTier)
        } else if let Some(runtime) = input.policy_runtime.and_then(non_empty) {
            (runtime, RuntimeSource::Policy)
        } else {
            ("claude".to_owned(), RuntimeSource::EmptyDefault)
        }
    } else {
        ("claude".to_owned(), RuntimeSource::EmptyDefault)
    };

    // Go carries flag, environment, and routing values verbatim into its
    // case-sensitive executor registry. Only an exact registered value runs;
    // differently-cased or whitespace-padded values fall back to Claude.
    let supported = SUPPORTED_RUNTIMES.contains(&requested.as_str());
    RuntimeResolution {
        runtime: if supported {
            requested
        } else {
            "claude".to_owned()
        },
        source,
        unknown_fell_back_to_claude: !supported,
    }
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

/// Inputs to pure model precedence resolution.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelResolutionInput {
    pub forced_model: Option<String>,
    pub tier: String,
    pub effective_runtime: String,
    pub routing_map: RoutingMap,
    /// Frozen built-in `(tier, runtime)` observation supplied by the caller.
    pub built_in_model: String,
}

/// Resolves model precedence without provider or credential access.
#[must_use]
pub fn resolve_model(input: ModelResolutionInput) -> String {
    if let Some(model) = input.forced_model.and_then(non_empty) {
        return model;
    }
    if let Some(entry) = input.routing_map.model_tiers.get(&input.tier) {
        if entry.runtime == input.effective_runtime && !entry.model.is_empty() {
            return entry.model.clone();
        }
    }
    input.built_in_model
}

/// Invalid explicit stall-timeout configuration.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ConfigError {
    #[error("--stall-timeout {value:?} is not a valid positive Go duration")]
    InvalidStallFlag { value: String },
    #[error("ORCHESTRATOR_STALL_TIMEOUT {value:?} is not a valid positive Go duration")]
    InvalidStallEnvironment { value: String },
}

/// Inputs to pure stall precedence resolution.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StallResolutionInput {
    pub phase_timeout: Option<Duration>,
    pub flag_value: Option<String>,
    pub environment_value: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StallSource {
    Phase,
    Flag,
    Environment,
    WorkerDefault,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StallTimeoutValue {
    Duration(Duration),
    WorkerDefault,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StallTimeoutResolution {
    pub value: StallTimeoutValue,
    pub source: StallSource,
}

/// Resolves stall timeout precedence, intentionally rejecting bad environment values.
pub fn resolve_stall_timeout(
    input: StallResolutionInput,
) -> Result<StallTimeoutResolution, ConfigError> {
    if let Some(duration) = input.phase_timeout.filter(|value| !value.is_zero()) {
        return Ok(stall(duration, StallSource::Phase));
    }
    if let Some(raw) = input.flag_value {
        return parse_go_duration(&raw).map_or_else(
            || Err(ConfigError::InvalidStallFlag { value: raw }),
            |duration| Ok(stall(duration, StallSource::Flag)),
        );
    }
    if let Some(raw) = input.environment_value {
        return parse_go_duration(&raw).map_or_else(
            || Err(ConfigError::InvalidStallEnvironment { value: raw }),
            |duration| Ok(stall(duration, StallSource::Environment)),
        );
    }
    Ok(StallTimeoutResolution {
        value: StallTimeoutValue::WorkerDefault,
        source: StallSource::WorkerDefault,
    })
}

fn stall(duration: Duration, source: StallSource) -> StallTimeoutResolution {
    StallTimeoutResolution {
        value: StallTimeoutValue::Duration(duration),
        source,
    }
}

/// Advisor configuration boundary, ported from the snapshot's
/// `internal/config/advisor.go`. Full `advisorbridge.UnattendedConfig`
/// (harness identity, trigger policy, limits) is out of scope — 6.8k LOC of
/// PROCESS-class Go with no `ORC-*` ledger contract (`PORT-ORDER.md` §6) —
/// but `EmergencyStop` is kept as an explicit field because it is a
/// fail-safe default (`DefaultUnattendedConfig`, `unattended.go:125`:
/// `EmergencyStop: true`), not scope surface; dropping it silently would
/// misrepresent Go's safe default rather than merely defer unmodeled scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdvisorConfig {
    /// Mirrors `advisorbridge.UnattendedConfig.Enabled`
    /// (`unattended.go:113`). Go's own zero value is `false`
    /// (`DefaultUnattendedConfig`, `unattended.go:125`).
    pub unattended_glm_enabled: bool,
    /// Mirrors `advisorbridge.UnattendedConfig.EmergencyStop`
    /// (`unattended.go:114`). Go's default is `true`
    /// (`DefaultUnattendedConfig`, `unattended.go:125`).
    pub emergency_stop: bool,
}

impl Default for AdvisorConfig {
    fn default() -> Self {
        Self::default_off()
    }
}

impl AdvisorConfig {
    /// Go's `DefaultAdvisorConfig()` / `DefaultUnattendedConfig()` boundary:
    /// the advisor is off unless a config stanza explicitly turns it on,
    /// but `emergency_stop` stays `true` per Go's own default.
    #[must_use]
    pub const fn default_off() -> Self {
        Self {
            unattended_glm_enabled: false,
            emergency_stop: true,
        }
    }
}

/// Pure resolution mirroring Go's `LoadAdvisorConfig` contract — "missing
/// file/stanza is default-off; a malformed stanza returns the safe defaults"
/// (`advisor.go:16-19`) — without reading `config.yaml` or depending on the
/// `advisorbridge` YAML schema here. The caller is responsible for parsing
/// `advisor.unattended_glm.{enabled,emergency_stop}` out of `config.yaml`
/// (or determining the file/stanza is absent) and passing the results in;
/// `None` covers both the missing-file case (`os.ErrNotExist`) and a
/// malformed stanza, both of which Go resolves to the same safe default
/// (`enabled: false`, `emergency_stop: true`).
#[must_use]
pub fn resolve_advisor_config(
    enabled_field: Option<bool>,
    emergency_stop_field: Option<bool>,
) -> AdvisorConfig {
    AdvisorConfig {
        unattended_glm_enabled: enabled_field.unwrap_or(false),
        emergency_stop: emergency_stop_field.unwrap_or(true),
    }
}

/// Parses the positive subset of Go's `time.ParseDuration` grammar.
pub(crate) fn parse_go_duration(raw: &str) -> Option<Duration> {
    let raw = raw.strip_prefix('+').unwrap_or(raw);
    if raw.is_empty() || raw.starts_with(['+', '-']) {
        return None;
    }
    let bytes = raw.as_bytes();
    let mut cursor = 0;
    let mut total_nanos = 0_u128;
    let mut components = 0;
    while cursor < bytes.len() {
        let number_start = cursor;
        let mut dot_seen = false;
        let mut digit_seen = false;
        while cursor < bytes.len() {
            match bytes[cursor] {
                b'0'..=b'9' => {
                    digit_seen = true;
                    cursor += 1;
                }
                b'.' if !dot_seen => {
                    dot_seen = true;
                    cursor += 1;
                }
                _ => break,
            }
        }
        if !digit_seen || cursor == bytes.len() {
            return None;
        }
        let number = &raw[number_start..cursor];
        let units = [
            ("ns", 1_u128),
            ("us", 1_000_u128),
            ("µs", 1_000_u128),
            ("μs", 1_000_u128),
            ("ms", 1_000_000_u128),
            ("s", 1_000_000_000_u128),
            ("m", 60_000_000_000_u128),
            ("h", 3_600_000_000_000_u128),
        ];
        let (unit, multiplier) = units
            .iter()
            .find(|(unit, _)| raw[cursor..].starts_with(unit))?;
        cursor += unit.len();
        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
        let whole = if whole.is_empty() {
            0
        } else {
            whole.parse::<u128>().ok()?
        };
        let whole_nanos = whole.checked_mul(*multiplier)?;
        // Match Go's `leadingFraction` and its deliberate float64 conversion.
        // The conversion is needed for nanosecond-accurate long fractions of
        // hours; exact integer division differs by one nanosecond for values
        // such as `0.3333333333333333333h`.
        let mut fraction_value = 0_u64;
        let mut fraction_scale = 1_f64;
        let mut fraction_overflowed = false;
        for digit in fraction.bytes() {
            if fraction_overflowed {
                continue;
            }
            if fraction_value > (i64::MAX as u64) / 10 {
                fraction_overflowed = true;
                continue;
            }
            let candidate = fraction_value * 10 + u64::from(digit - b'0');
            if candidate > (1_u64 << 63) {
                fraction_overflowed = true;
                continue;
            }
            fraction_value = candidate;
            fraction_scale *= 10_f64;
        }
        let fraction_nanos = if fraction_value == 0 {
            0
        } else {
            (fraction_value as f64 * (*multiplier as f64 / fraction_scale)) as u128
        };
        total_nanos = total_nanos
            .checked_add(whole_nanos)?
            .checked_add(fraction_nanos)?;
        components += 1;
    }
    if components == 0 || total_nanos == 0 || total_nanos > i64::MAX as u128 {
        return None;
    }
    Some(Duration::from_nanos(u64::try_from(total_nanos).ok()?))
}
