//! Pure, replayable policy for usage-aware route selection.
//!
//! This module deliberately owns no clocks, processes, credentials, provider clients, or
//! persistence. Callers must normalize those concerns into the versioned inputs below. The same
//! policy and input therefore always produce the same decision.

use crate::{MissionId, PhaseId};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::collections::BTreeSet;
use thiserror::Error;

/// Schema version accepted by the adaptive-routing input, decision, and replay record.
pub const ADAPTIVE_ROUTING_SCHEMA_V1: u32 = 1;
/// Policy version implemented by this module.
pub const ADAPTIVE_POLICY_VERSION_V1: u32 = 1;
/// Largest positive duration accepted by version one (366 days in milliseconds).
pub const ADAPTIVE_MAX_DURATION_MS_V1: u64 = 31_622_400_000;

const BASIS_POINTS_SCALE: u16 = 10_000;
const MAX_ROUTE_TEXT_BYTES: usize = 256;
const MAX_SOURCE_TEXT_BYTES: usize = 128;
const MAX_PROVIDER_RESERVES: usize = 64;
const MAX_CANDIDATES: usize = 128;
const MAX_USAGE_SNAPSHOTS: usize = 256;
const MAX_WINDOWS_PER_SNAPSHOT: usize = 32;
const MAX_MODELS_PER_WINDOW: usize = 128;
const MAX_CAPABILITIES: usize = 64;
const MAX_FIXED_PROVENANCES: usize = 6;
const MAX_TOTAL_WINDOWS: usize = 512;
const MAX_TOTAL_MODEL_REFERENCES: usize = 4_096;
const MAX_TOTAL_ROUTING_ELEMENTS: usize = 8_192;
const MAX_TOTAL_ROUTING_TEXT_BYTES: usize = 1_048_576;
const MAX_TOTAL_CONSIDERED_SNAPSHOT_REFERENCES: usize = 8_192;
const MAX_USAGE_WINDOW_EVALUATIONS: usize = 32_768;

/// Fail-closed validation and replay errors from the adaptive-routing kernel.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AdaptiveRoutingError {
    /// A basis-point value exceeded the inclusive range `0..=10_000`.
    #[error("basis-point value {found} exceeds 10000")]
    BasisPointsOutOfRange {
        /// Rejected integer value.
        found: u16,
    },
    /// A duration that must be positive was zero.
    #[error("duration must be greater than zero")]
    ZeroDuration,
    /// A duration exceeded the bounded v1 policy horizon.
    #[error("duration {found}ms exceeds the v1 maximum of {maximum}ms")]
    DurationOutOfRange {
        /// Rejected duration in milliseconds.
        found: u64,
        /// Largest duration accepted by v1.
        maximum: u64,
    },
    /// A schema-bearing value used an unsupported version.
    #[error("unsupported {surface} schema version {found}; expected {expected}")]
    UnsupportedSchemaVersion {
        /// Stable name of the rejected surface.
        surface: &'static str,
        /// Version supplied by the caller.
        found: u32,
        /// Version accepted by this implementation.
        expected: u32,
    },
    /// The policy used an unsupported semantic version.
    #[error("unsupported adaptive policy version {found}; expected {expected}")]
    UnsupportedPolicyVersion {
        /// Version supplied by the caller.
        found: u32,
        /// Version accepted by this implementation.
        expected: u32,
    },
    /// A bounded textual field was empty, malformed, or too long.
    #[error("invalid {field}: {reason}")]
    InvalidText {
        /// Stable field name without the rejected value.
        field: &'static str,
        /// Stable validation reason.
        reason: &'static str,
    },
    /// A numeric or collection invariant was not satisfied.
    #[error("invalid {field}: {reason}")]
    InvalidInvariant {
        /// Stable field name.
        field: &'static str,
        /// Stable validation reason.
        reason: &'static str,
    },
    /// Two candidates used the same stable identifier.
    #[error("duplicate adaptive-routing candidate identifier")]
    DuplicateCandidate,
    /// Two candidate identifiers described the same executable route.
    #[error("duplicate adaptive-routing executable route")]
    DuplicateExecutableRoute,
    /// Two usage snapshots used the same stable identifier.
    #[error("duplicate adaptive-routing usage snapshot identifier")]
    DuplicateSnapshot,
    /// Two windows in one snapshot used the same stable limit identifier.
    #[error("duplicate usage limit identifier in one snapshot")]
    DuplicateUsageLimit,
    /// A policy contained more than one reserve for the same provider/account pair.
    #[error("duplicate provider/account reserve")]
    DuplicateProviderReserve,
    /// A candidate's provider/account pair had no explicit reserve in the policy.
    #[error("candidate provider/account is missing an explicit reserve")]
    MissingProviderReserve,
    /// Integer score arithmetic could not be represented safely.
    #[error("adaptive-routing score arithmetic overflowed")]
    ScoreOverflow,
    /// A replay record's expected decision did not match recomputation.
    #[error("adaptive-routing replay decision mismatch")]
    ReplayMismatch,
}

fn validate_stable_id(field: &'static str, value: &str) -> Result<(), AdaptiveRoutingError> {
    if value.is_empty() {
        return Err(AdaptiveRoutingError::InvalidText {
            field,
            reason: "identifier is empty",
        });
    }
    if value.len() > MAX_SOURCE_TEXT_BYTES {
        return Err(AdaptiveRoutingError::InvalidText {
            field,
            reason: "identifier is too long",
        });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AdaptiveRoutingError::InvalidText {
            field,
            reason: "only ASCII letters, digits, '-' and '_' are allowed",
        });
    }
    Ok(())
}

fn validate_bounded_text(
    field: &'static str,
    value: &str,
    maximum_bytes: usize,
) -> Result<(), AdaptiveRoutingError> {
    if value.trim().is_empty() {
        return Err(AdaptiveRoutingError::InvalidText {
            field,
            reason: "value is empty",
        });
    }
    if value.trim() != value {
        return Err(AdaptiveRoutingError::InvalidText {
            field,
            reason: "leading or trailing whitespace is not allowed",
        });
    }
    if value.len() > maximum_bytes {
        return Err(AdaptiveRoutingError::InvalidText {
            field,
            reason: "value is too long",
        });
    }
    if !value.bytes().all(|byte| matches!(byte, 0x20..=0x7E)) {
        return Err(AdaptiveRoutingError::InvalidText {
            field,
            reason: "only printable ASCII bytes are allowed",
        });
    }
    Ok(())
}

macro_rules! stable_id {
    ($name:ident, $field:literal, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Validates and constructs the identifier.
            pub fn new(value: impl Into<String>) -> Result<Self, AdaptiveRoutingError> {
                let value = value.into();
                validate_stable_id($field, &value)?;
                Ok(Self(value))
            }

            /// Returns the stable identifier text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = AdaptiveRoutingError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

stable_id!(
    CandidateIdV1,
    "candidate_id",
    "Stable identifier used for deterministic candidate ordering."
);
stable_id!(
    SnapshotIdV1,
    "snapshot_id",
    "Stable identifier for a normalized usage observation."
);
stable_id!(
    ProviderIdV1,
    "provider_id",
    "Stable provider-family identifier."
);
stable_id!(
    RuntimeIdV1,
    "runtime_id",
    "Stable runtime-family identifier used for safe continuation decisions."
);
stable_id!(
    UsageAccountIdV1,
    "usage_account_id",
    "Stable non-secret account identity used to bind routes, reserves, and snapshots."
);
stable_id!(
    CapabilityIdV1,
    "capability_id",
    "Stable capability identifier used by hard constraints."
);
stable_id!(
    UsageLimitIdV1,
    "usage_limit_id",
    "Stable identifier for one provider usage window."
);
stable_id!(
    UsageReasonCodeV1,
    "usage_reason_code",
    "Bounded diagnostic reason emitted by a normalized usage adapter."
);
stable_id!(
    UsageSourceIdV1,
    "usage_source_id",
    "Stable non-sensitive identifier for a normalized usage adapter."
);
stable_id!(
    UsageWindowKindV1,
    "usage_window_kind",
    "Stable non-sensitive identifier for a provider usage-window kind."
);

/// Integer percentage in the inclusive range `0..=10_000`, where 10_000 is 100%.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct BasisPointsV1(u16);

impl BasisPointsV1 {
    /// Validates and constructs a basis-point value.
    pub const fn new(value: u16) -> Result<Self, AdaptiveRoutingError> {
        if value > BASIS_POINTS_SCALE {
            Err(AdaptiveRoutingError::BasisPointsOutOfRange { found: value })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the normalized integer value.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for BasisPointsV1 {
    type Error = AdaptiveRoutingError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<BasisPointsV1> for u16 {
    fn from(value: BasisPointsV1) -> Self {
        value.get()
    }
}

/// UTC wall-clock milliseconds supplied by an adapter.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct UtcMillisV1(u64);

impl UtcMillisV1 {
    /// Constructs a timestamp. The pure kernel does not consult a clock.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the timestamp integer.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Positive duration in milliseconds.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct DurationMillisV1(u64);

impl DurationMillisV1 {
    /// Validates and constructs a positive duration.
    pub const fn new(value: u64) -> Result<Self, AdaptiveRoutingError> {
        if value == 0 {
            Err(AdaptiveRoutingError::ZeroDuration)
        } else if value > ADAPTIVE_MAX_DURATION_MS_V1 {
            Err(AdaptiveRoutingError::DurationOutOfRange {
                found: value,
                maximum: ADAPTIVE_MAX_DURATION_MS_V1,
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the duration integer.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for DurationMillisV1 {
    type Error = AdaptiveRoutingError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<DurationMillisV1> for u64 {
    fn from(value: DurationMillisV1) -> Self {
        value.get()
    }
}

/// Exact executable route. No hidden model or effort defaults are applied by this module.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RouteTargetV1 {
    /// Stable identity used for tie-breaking and incumbent matching.
    pub candidate_id: CandidateIdV1,
    /// Provider family that owns the route's usage budget.
    pub provider: ProviderIdV1,
    /// Non-secret account identity whose quota governs this route.
    pub usage_account_id: UsageAccountIdV1,
    /// Runtime adapter name.
    pub runtime: RuntimeIdV1,
    /// Exact provider model name.
    pub model: String,
    /// Exact effort setting, or `None` when the runtime has no effort dimension.
    pub effort: Option<String>,
}

impl RouteTargetV1 {
    fn validate(&self) -> Result<(), AdaptiveRoutingError> {
        validate_bounded_text("route.model", &self.model, MAX_ROUTE_TEXT_BYTES)?;
        if let Some(effort) = &self.effort {
            validate_bounded_text("route.effort", effort, MAX_ROUTE_TEXT_BYTES)?;
        }
        Ok(())
    }

    fn same_executable_target(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.usage_account_id == other.usage_account_id
            && self.runtime == other.runtime
            && self.model == other.model
            && self.effort == other.effort
    }
}

/// Explicit origin retained after the existing resolver composes a fixed route.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FixedAuthorityProvenanceV1 {
    /// Runtime authored directly on the mission or phase.
    AuthoredRuntime,
    /// Explicit runtime command-line flag.
    RuntimeFlag,
    /// Explicit runtime environment setting.
    EnvironmentRuntime,
    /// Explicit model command-line flag.
    ModelFlag,
    /// Provider fixed by a non-adaptive configured mode.
    FixedProviderMode,
    /// Existing legacy route selection.
    LegacyMode,
}

fn deserialize_unique_fixed_provenances<'de, D>(
    deserializer: D,
) -> Result<BTreeSet<FixedAuthorityProvenanceV1>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<FixedAuthorityProvenanceV1>::deserialize(deserializer)?;
    let mut unique = BTreeSet::new();
    for value in values {
        if !unique.insert(value) {
            return Err(de::Error::custom("duplicate fixed authority provenance"));
        }
    }
    Ok(unique)
}

/// One route asserted by a fixed authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FixedRouteV1 {
    /// Version of the adapter-side fixed-route composition contract.
    pub composition_version: u32,
    /// All orthogonal sources used by the adapter to compose the exact route.
    #[serde(deserialize_with = "deserialize_unique_fixed_provenances")]
    pub provenances: BTreeSet<FixedAuthorityProvenanceV1>,
    /// Exact route asserted by that source.
    pub route: RouteTargetV1,
}

impl FixedRouteV1 {
    fn validate(&self) -> Result<(), AdaptiveRoutingError> {
        validate_schema("fixed route composition", self.composition_version)?;
        if self.provenances.is_empty() {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "fixed_route.provenances",
                reason: "resolved fixed route must retain at least one authority source",
            });
        }
        if self.provenances.len() > MAX_FIXED_PROVENANCES {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "fixed_route.provenances",
                reason: "too many fixed-route provenance sources",
            });
        }
        self.route.validate()
    }
}

/// All routing authority supplied by the composition root.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingAuthorityV1 {
    /// Exact fixed route already composed by the existing precedence resolver, when present.
    pub resolved_fixed_route: Option<FixedRouteV1>,
    /// Exact result of the unchanged legacy router, used for fallback and shadow dispatch.
    pub legacy_route: RouteTargetV1,
}

/// Whether a candidate may be chosen without an explicit human request.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomaticEligibilityV1 {
    /// Candidate may participate in adaptive selection.
    Automatic,
    /// Candidate may be recommended to a human but never automatically dispatched.
    AdvisoryOnly,
    /// Candidate may only be selected by an explicit fixed authority.
    ExplicitOnly,
}

/// Coarse model quality tier used only as a hard floor.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityTierV1 {
    /// Lowest-cost quality class.
    Economy,
    /// General-purpose quality class.
    Standard,
    /// High-quality class.
    Premium,
    /// Highest configured quality class.
    Frontier,
}

impl QualityTierV1 {
    const fn rank(self) -> u8 {
        match self {
            Self::Economy => 0,
            Self::Standard => 1,
            Self::Premium => 2,
            Self::Frontier => 3,
        }
    }
}

/// Normalized task priority. P0 alone is entitled to consume configured reserve.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriorityV1 {
    /// Critical work allowed to consume reserve.
    P0,
    /// High-priority work.
    P1,
    /// Normal-priority work.
    P2,
    /// Deferrable work.
    P3,
}

/// Hard requirements evaluated before any candidate score is calculated.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RouteRequirementsV1 {
    /// Mission priority for reserve entitlement.
    pub priority: TaskPriorityV1,
    /// Capabilities every selected candidate must advertise.
    pub required_capabilities: BTreeSet<CapabilityIdV1>,
    /// Lowest permitted quality tier.
    pub minimum_quality: QualityTierV1,
}

/// Scorable candidate normalized by configuration adapters.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveCandidateV1 {
    /// Exact dispatch route.
    pub route: RouteTargetV1,
    /// Hard capabilities supported by the route.
    pub capabilities: BTreeSet<CapabilityIdV1>,
    /// Whether automatic dispatch is authorized.
    pub automatic_eligibility: AutomaticEligibilityV1,
    /// Quality class checked before scoring.
    pub quality: QualityTierV1,
    /// Precomputed task-fit component.
    pub task_fit_bps: BasisPointsV1,
    /// Precomputed latency component, where a larger value is better.
    pub latency_bps: BasisPointsV1,
    /// Explicit configured preference component.
    pub configured_preference_bps: BasisPointsV1,
}

/// Health of a normalized usage observation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageHealthV1 {
    /// Adapter considers the normalized observation complete and usable.
    Healthy,
    /// Adapter produced only a partial observation; the policy fails closed.
    Partial,
    /// Adapter explicitly considers the source unavailable.
    Unavailable,
}

/// One normalized quota window from a provider observation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsageWindowV1 {
    /// Stable limit identity within the snapshot.
    pub limit_id: UsageLimitIdV1,
    /// Provider-defined stable kind such as a rolling or weekly window.
    pub kind: UsageWindowKindV1,
    /// Empty means provider-wide; otherwise the window only governs exact model names here.
    pub applicable_models: BTreeSet<String>,
    /// Consumed proportion.
    pub used_bps: BasisPointsV1,
    /// Remaining proportion.
    pub remaining_bps: BasisPointsV1,
    /// Adapter-normalized recent capacity for this window, where a larger value is better.
    pub recent_capacity_bps: BasisPointsV1,
    /// Time at which this observation becomes invalid because the quota resets.
    pub resets_at_utc_ms: UtcMillisV1,
    /// Exact declared window duration in milliseconds.
    pub duration_ms: DurationMillisV1,
}

impl UsageWindowV1 {
    fn validate(&self, observed_at: UtcMillisV1) -> Result<(), AdaptiveRoutingError> {
        let total = u32::from(self.used_bps.get()) + u32::from(self.remaining_bps.get());
        if total != u32::from(BASIS_POINTS_SCALE) {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "usage_window.basis_points",
                reason: "used and remaining basis points must total 10000",
            });
        }
        if self.resets_at_utc_ms <= observed_at {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "usage_window.resets_at_utc_ms",
                reason: "reset must be later than observation time",
            });
        }
        let observed_horizon = self.resets_at_utc_ms.get() - observed_at.get();
        if observed_horizon > self.duration_ms.get() {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "usage_window.resets_at_utc_ms",
                reason: "reset horizon exceeds declared window duration",
            });
        }
        if self.applicable_models.len() > MAX_MODELS_PER_WINDOW {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "usage_window.applicable_models",
                reason: "too many applicable model names",
            });
        }
        for model in &self.applicable_models {
            validate_bounded_text("usage_window.applicable_model", model, MAX_ROUTE_TEXT_BYTES)?;
        }
        Ok(())
    }
}

/// Normalized usage snapshot. Raw provider payloads are intentionally not representable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsageSnapshotV1 {
    /// Snapshot schema version.
    pub schema_version: u32,
    /// Stable observation identity.
    pub snapshot_id: SnapshotIdV1,
    /// Provider family governed by this observation.
    pub provider: ProviderIdV1,
    /// Non-secret account identity used for exact route matching.
    pub usage_account_id: UsageAccountIdV1,
    /// Stable adapter/source name.
    pub source: UsageSourceIdV1,
    /// Time represented by the provider data.
    pub observed_at_utc_ms: UtcMillisV1,
    /// Time the local adapter received the data.
    pub received_at_utc_ms: UtcMillisV1,
    /// Maximum adapter-declared lifetime.
    pub ttl_ms: DurationMillisV1,
    /// Adapter confidence used as the health score component.
    pub confidence_bps: BasisPointsV1,
    /// Explicit source health.
    pub health: UsageHealthV1,
    /// Stable diagnostic reason from the adapter, never a raw payload.
    pub reason_code: Option<UsageReasonCodeV1>,
    /// Provider usage windows.
    pub windows: Vec<UsageWindowV1>,
}

impl UsageSnapshotV1 {
    fn validate(&self) -> Result<(), AdaptiveRoutingError> {
        validate_schema("usage snapshot", self.schema_version)?;
        if self.observed_at_utc_ms > self.received_at_utc_ms {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "usage_snapshot.observed_at_utc_ms",
                reason: "observation cannot be later than receipt",
            });
        }
        match self.health {
            UsageHealthV1::Healthy => {
                if self.confidence_bps.get() == 0 {
                    return Err(AdaptiveRoutingError::InvalidInvariant {
                        field: "usage_snapshot.confidence_bps",
                        reason: "a healthy snapshot must have non-zero confidence",
                    });
                }
                if self.windows.is_empty() {
                    return Err(AdaptiveRoutingError::InvalidInvariant {
                        field: "usage_snapshot.windows",
                        reason: "a healthy snapshot requires at least one window",
                    });
                }
            }
            UsageHealthV1::Partial | UsageHealthV1::Unavailable => {
                if self.reason_code.is_none() {
                    return Err(AdaptiveRoutingError::InvalidInvariant {
                        field: "usage_snapshot.reason_code",
                        reason: "an unhealthy snapshot requires a stable reason code",
                    });
                }
            }
        }
        if self.windows.len() > MAX_WINDOWS_PER_SNAPSHOT {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "usage_snapshot.windows",
                reason: "too many usage windows",
            });
        }
        let mut limits = BTreeSet::new();
        for window in &self.windows {
            if !limits.insert(window.limit_id.clone()) {
                return Err(AdaptiveRoutingError::DuplicateUsageLimit);
            }
            window.validate(self.observed_at_utc_ms)?;
        }
        Ok(())
    }
}

/// Explicit reserve assigned to one provider/account pair.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderReserveV1 {
    /// Provider family.
    pub provider: ProviderIdV1,
    /// Non-secret account identity within the provider.
    pub usage_account_id: UsageAccountIdV1,
    /// Headroom protected from all priorities except P0.
    pub reserve_bps: BasisPointsV1,
}

/// Integer weights for deterministic candidate scoring.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScoreWeightsV1 {
    /// Task-fit component weight.
    pub task_fit_bps: BasisPointsV1,
    /// Post-reserve usable-headroom component weight.
    pub usable_headroom_bps: BasisPointsV1,
    /// Conservative reset relief across governing windows, where a larger value is better.
    pub reset_proximity_bps: BasisPointsV1,
    /// Adapter-normalized recent-capacity component weight.
    pub recent_capacity_bps: BasisPointsV1,
    /// Latency component weight.
    pub latency_bps: BasisPointsV1,
    /// Usage-observation health/confidence component weight.
    pub health_bps: BasisPointsV1,
    /// Configured preference component weight.
    pub configured_preference_bps: BasisPointsV1,
}

impl ScoreWeightsV1 {
    fn validate(&self) -> Result<(), AdaptiveRoutingError> {
        let total = [
            self.task_fit_bps,
            self.usable_headroom_bps,
            self.reset_proximity_bps,
            self.recent_capacity_bps,
            self.latency_bps,
            self.health_bps,
            self.configured_preference_bps,
        ]
        .into_iter()
        .map(BasisPointsV1::get)
        .map(u32::from)
        .sum::<u32>();
        if total != u32::from(BASIS_POINTS_SCALE) {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "adaptive_policy.score_weights",
                reason: "weights must total 10000 basis points",
            });
        }
        Ok(())
    }
}

/// Fallback behavior when no candidate has a trustworthy usage observation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownUsagePolicyV1 {
    /// Never produces an adaptive fallback recommendation; enforcement defers on adverse evidence.
    Defer,
    /// Dispatch only the canonical configured candidate matching the legacy executable target,
    /// and only when that candidate's rejection is unknown usage.
    StaticFallback,
    /// Hold the incumbent only when it passes non-usage constraints and nothing is scorable.
    HoldIncumbent,
}

/// Whether adaptive output controls dispatch or is recorded as a recommendation only.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdaptiveEvaluationModeV1 {
    /// Dispatch the selected adaptive recommendation.
    Enforce,
    /// Compute the recommendation but dispatch the exact legacy route.
    Shadow,
}

/// Fully explicit version-one adaptive policy. This type has no production defaults.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptivePolicyV1 {
    /// Semantic policy version.
    pub policy_version: u32,
    /// Maximum age accepted even when an adapter advertises a longer TTL.
    pub maximum_snapshot_age_ms: DurationMillisV1,
    /// Explicit reserve for every provider represented by an adaptive candidate.
    pub provider_reserves: Vec<ProviderReserveV1>,
    /// Deterministic integer scoring weights.
    pub score_weights: ScoreWeightsV1,
    /// Minimum inclusive score advantage required to leave an eligible incumbent.
    pub switch_margin_bps: BasisPointsV1,
    /// Fail-closed behavior for unknown usage.
    pub unknown_usage_policy: UnknownUsagePolicyV1,
    /// Enforced or shadow-only evaluation.
    pub evaluation_mode: AdaptiveEvaluationModeV1,
}

impl AdaptivePolicyV1 {
    fn validate(&self) -> Result<(), AdaptiveRoutingError> {
        if self.policy_version != ADAPTIVE_POLICY_VERSION_V1 {
            return Err(AdaptiveRoutingError::UnsupportedPolicyVersion {
                found: self.policy_version,
                expected: ADAPTIVE_POLICY_VERSION_V1,
            });
        }
        self.score_weights.validate()?;
        self.validate_provider_reserves()
    }

    fn validate_provider_reserves(&self) -> Result<(), AdaptiveRoutingError> {
        if self.provider_reserves.len() > MAX_PROVIDER_RESERVES {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "adaptive_policy.provider_reserves",
                reason: "too many provider reserves",
            });
        }
        let mut provider_accounts = BTreeSet::new();
        for reserve in &self.provider_reserves {
            if !provider_accounts
                .insert((reserve.provider.clone(), reserve.usage_account_id.clone()))
            {
                return Err(AdaptiveRoutingError::DuplicateProviderReserve);
            }
        }
        Ok(())
    }

    fn reserve_for(
        &self,
        provider: &ProviderIdV1,
        usage_account_id: &UsageAccountIdV1,
    ) -> Option<BasisPointsV1> {
        self.provider_reserves
            .iter()
            .find(|reserve| {
                &reserve.provider == provider && &reserve.usage_account_id == usage_account_id
            })
            .map(|reserve| reserve.reserve_bps)
    }

    fn canonicalized(&self) -> Self {
        let mut canonical = self.clone();
        canonical.provider_reserves.sort_by(|left, right| {
            left.provider
                .cmp(&right.provider)
                .then_with(|| left.usage_account_id.cmp(&right.usage_account_id))
        });
        canonical
    }
}

/// Complete deterministic input to one routing decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingInputV1 {
    /// Input schema version.
    pub schema_version: u32,
    /// Mission identity used for audit correlation, never scoring.
    pub mission_id: MissionId,
    /// Phase identity used for audit correlation, never scoring.
    pub phase_id: PhaseId,
    /// One-based attempt number.
    pub attempt: u32,
    /// Evaluation time captured by the caller.
    pub evaluated_at_utc_ms: UtcMillisV1,
    /// Fixed authorities plus the legacy shadow/canonical-unknown-fallback target.
    pub authority: RoutingAuthorityV1,
    /// Hard task requirements.
    pub requirements: RouteRequirementsV1,
    /// Candidate catalog.
    pub candidates: Vec<AdaptiveCandidateV1>,
    /// Normalized usage observations. Raw provider payloads have no field here.
    pub usage_snapshots: Vec<UsageSnapshotV1>,
    /// Current candidate identity, when one exists.
    pub incumbent_candidate_id: Option<CandidateIdV1>,
    /// Existing runtime family only. Session identifiers are intentionally unrepresentable.
    pub existing_session_runtime: Option<RuntimeIdV1>,
}

#[derive(Default)]
struct RoutingBudget {
    elements: usize,
    text_bytes: usize,
    windows: usize,
    model_references: usize,
}

impl RoutingBudget {
    fn add_elements(&mut self, count: usize) -> Result<(), AdaptiveRoutingError> {
        self.elements =
            self.elements
                .checked_add(count)
                .ok_or(AdaptiveRoutingError::InvalidInvariant {
                    field: "adaptive_routing.aggregate_elements",
                    reason: "aggregate element count overflowed",
                })?;
        if self.elements > MAX_TOTAL_ROUTING_ELEMENTS {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "adaptive_routing.aggregate_elements",
                reason: "too many aggregate routing elements",
            });
        }
        Ok(())
    }

    fn add_text(&mut self, value: &str) -> Result<(), AdaptiveRoutingError> {
        self.text_bytes = self.text_bytes.checked_add(value.len()).ok_or(
            AdaptiveRoutingError::InvalidInvariant {
                field: "adaptive_routing.aggregate_text_bytes",
                reason: "aggregate text byte count overflowed",
            },
        )?;
        if self.text_bytes > MAX_TOTAL_ROUTING_TEXT_BYTES {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "adaptive_routing.aggregate_text_bytes",
                reason: "too many aggregate routing text bytes",
            });
        }
        Ok(())
    }

    fn add_windows(&mut self, count: usize) -> Result<(), AdaptiveRoutingError> {
        self.windows =
            self.windows
                .checked_add(count)
                .ok_or(AdaptiveRoutingError::InvalidInvariant {
                    field: "adaptive_routing.total_windows",
                    reason: "aggregate usage-window count overflowed",
                })?;
        if self.windows > MAX_TOTAL_WINDOWS {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "adaptive_routing.total_windows",
                reason: "too many aggregate usage windows",
            });
        }
        Ok(())
    }

    fn add_model_references(&mut self, count: usize) -> Result<(), AdaptiveRoutingError> {
        self.model_references = self.model_references.checked_add(count).ok_or(
            AdaptiveRoutingError::InvalidInvariant {
                field: "adaptive_routing.total_model_references",
                reason: "aggregate model-reference count overflowed",
            },
        )?;
        if self.model_references > MAX_TOTAL_MODEL_REFERENCES {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "adaptive_routing.total_model_references",
                reason: "too many aggregate model references",
            });
        }
        Ok(())
    }
}

fn account_route_budget(
    budget: &mut RoutingBudget,
    route: &RouteTargetV1,
) -> Result<(), AdaptiveRoutingError> {
    budget.add_elements(1)?;
    budget.add_text(route.candidate_id.as_str())?;
    budget.add_text(route.provider.as_str())?;
    budget.add_text(route.usage_account_id.as_str())?;
    budget.add_text(route.runtime.as_str())?;
    budget.add_text(&route.model)?;
    if let Some(effort) = &route.effort {
        budget.add_text(effort)?;
    }
    Ok(())
}

fn account_candidate_budget(
    budget: &mut RoutingBudget,
    candidate: &AdaptiveCandidateV1,
) -> Result<(), AdaptiveRoutingError> {
    account_route_budget(budget, &candidate.route)?;
    budget.add_elements(candidate.capabilities.len())?;
    for capability in &candidate.capabilities {
        budget.add_text(capability.as_str())?;
    }
    Ok(())
}

fn account_snapshot_budget(
    budget: &mut RoutingBudget,
    snapshot: &UsageSnapshotV1,
) -> Result<(), AdaptiveRoutingError> {
    budget.add_elements(1)?;
    budget.add_text(snapshot.snapshot_id.as_str())?;
    budget.add_text(snapshot.provider.as_str())?;
    budget.add_text(snapshot.usage_account_id.as_str())?;
    budget.add_text(snapshot.source.as_str())?;
    if let Some(reason_code) = &snapshot.reason_code {
        budget.add_text(reason_code.as_str())?;
    }
    budget.add_windows(snapshot.windows.len())?;
    budget.add_elements(snapshot.windows.len())?;
    for window in &snapshot.windows {
        budget.add_text(window.limit_id.as_str())?;
        budget.add_text(window.kind.as_str())?;
        budget.add_model_references(window.applicable_models.len())?;
        budget.add_elements(window.applicable_models.len())?;
        for model in &window.applicable_models {
            budget.add_text(model)?;
        }
    }
    Ok(())
}

impl RoutingInputV1 {
    fn validate_request_invariants(&self) -> Result<(), AdaptiveRoutingError> {
        validate_schema("routing input", self.schema_version)?;
        if self.attempt == 0 {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "routing_input.attempt",
                reason: "attempt is one-based",
            });
        }
        if let Some(fixed) = &self.authority.resolved_fixed_route {
            fixed.validate()?;
        }
        self.validate_collection_bounds()?;
        Ok(())
    }

    fn validate_collection_bounds(&self) -> Result<(), AdaptiveRoutingError> {
        if self.candidates.len() > MAX_CANDIDATES {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "routing_input.candidates",
                reason: "too many adaptive candidates",
            });
        }
        if self.usage_snapshots.len() > MAX_USAGE_SNAPSHOTS {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "routing_input.usage_snapshots",
                reason: "too many usage snapshots",
            });
        }
        if self.requirements.required_capabilities.len() > MAX_CAPABILITIES {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "routing_input.required_capabilities",
                reason: "too many required capabilities",
            });
        }
        for candidate in &self.candidates {
            if candidate.capabilities.len() > MAX_CAPABILITIES {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "adaptive_candidate.capabilities",
                    reason: "too many candidate capabilities",
                });
            }
        }
        for snapshot in &self.usage_snapshots {
            if snapshot.windows.len() > MAX_WINDOWS_PER_SNAPSHOT {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "usage_snapshot.windows",
                    reason: "too many usage windows",
                });
            }
            for window in &snapshot.windows {
                if window.applicable_models.len() > MAX_MODELS_PER_WINDOW {
                    return Err(AdaptiveRoutingError::InvalidInvariant {
                        field: "usage_window.applicable_models",
                        reason: "too many applicable model names",
                    });
                }
            }
        }
        let mut budget = RoutingBudget::default();
        budget.add_text(self.mission_id.as_str())?;
        budget.add_text(self.phase_id.as_str())?;
        budget.add_elements(self.requirements.required_capabilities.len())?;
        for capability in &self.requirements.required_capabilities {
            budget.add_text(capability.as_str())?;
        }
        account_route_budget(&mut budget, &self.authority.legacy_route)?;
        if let Some(fixed) = &self.authority.resolved_fixed_route {
            budget.add_elements(fixed.provenances.len())?;
            account_route_budget(&mut budget, &fixed.route)?;
        }
        if let Some(incumbent) = &self.incumbent_candidate_id {
            budget.add_text(incumbent.as_str())?;
        }
        if let Some(runtime) = &self.existing_session_runtime {
            budget.add_text(runtime.as_str())?;
        }
        for candidate in &self.candidates {
            account_candidate_budget(&mut budget, candidate)?;
        }
        for snapshot in &self.usage_snapshots {
            account_snapshot_budget(&mut budget, snapshot)?;
        }

        let mut considered_snapshot_references = 0_usize;
        let mut usage_window_evaluations = 0_usize;
        for candidate in &self.candidates {
            for snapshot in &self.usage_snapshots {
                if snapshot.provider == candidate.route.provider
                    && snapshot.usage_account_id == candidate.route.usage_account_id
                {
                    considered_snapshot_references = considered_snapshot_references
                        .checked_add(1)
                        .ok_or(AdaptiveRoutingError::InvalidInvariant {
                            field: "adaptive_routing.considered_snapshot_references",
                            reason: "considered snapshot-reference count overflowed",
                        })?;
                    if considered_snapshot_references > MAX_TOTAL_CONSIDERED_SNAPSHOT_REFERENCES {
                        return Err(AdaptiveRoutingError::InvalidInvariant {
                            field: "adaptive_routing.considered_snapshot_references",
                            reason: "too many considered snapshot references",
                        });
                    }
                    usage_window_evaluations = usage_window_evaluations
                        .checked_add(snapshot.windows.len())
                        .ok_or(AdaptiveRoutingError::InvalidInvariant {
                            field: "adaptive_routing.usage_window_evaluations",
                            reason: "usage evaluation work count overflowed",
                        })?;
                    if usage_window_evaluations > MAX_USAGE_WINDOW_EVALUATIONS {
                        return Err(AdaptiveRoutingError::InvalidInvariant {
                            field: "adaptive_routing.usage_window_evaluations",
                            reason: "too much aggregate usage evaluation work",
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_adaptive(&self, policy: &AdaptivePolicyV1) -> Result<(), AdaptiveRoutingError> {
        self.authority.legacy_route.validate()?;
        let mut candidate_ids = BTreeSet::new();
        let mut executable_routes = BTreeSet::new();
        for candidate in &self.candidates {
            candidate.route.validate()?;
            if !candidate_ids.insert(candidate.route.candidate_id.clone()) {
                return Err(AdaptiveRoutingError::DuplicateCandidate);
            }
            if !executable_routes.insert((
                candidate.route.provider.clone(),
                candidate.route.usage_account_id.clone(),
                candidate.route.runtime.clone(),
                candidate.route.model.clone(),
                candidate.route.effort.clone(),
            )) {
                return Err(AdaptiveRoutingError::DuplicateExecutableRoute);
            }
            if policy
                .reserve_for(&candidate.route.provider, &candidate.route.usage_account_id)
                .is_none()
            {
                return Err(AdaptiveRoutingError::MissingProviderReserve);
            }
        }

        let mut snapshot_ids = BTreeSet::new();
        for snapshot in &self.usage_snapshots {
            snapshot.validate()?;
            if !snapshot_ids.insert(snapshot.snapshot_id.clone()) {
                return Err(AdaptiveRoutingError::DuplicateSnapshot);
            }
        }
        if let Some(incumbent) = &self.incumbent_candidate_id {
            if !candidate_ids.contains(incumbent) {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_input.incumbent_candidate_id",
                    reason: "incumbent must identify a configured candidate",
                });
            }
        }
        Ok(())
    }
}

/// Stable reason why a candidate was rejected before dispatch selection.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateRejectionV1 {
    /// Candidate is advisory-only.
    AdvisoryOnly,
    /// Candidate requires an explicit fixed authority.
    ExplicitOnly,
    /// Candidate lacks at least one required capability.
    MissingCapability,
    /// Candidate is below the hard quality floor.
    BelowQualityFloor,
    /// No snapshot exists for the provider.
    MissingUsageSnapshot,
    /// Every applicable snapshot exceeded policy or adapter TTL.
    StaleUsageSnapshot,
    /// Evaluation time predates receipt of every applicable snapshot.
    UsageClockSkew,
    /// Every otherwise applicable observation has passed a reset boundary.
    ResetElapsed,
    /// Every applicable snapshot is partial or unavailable.
    UnhealthyUsage,
    /// Snapshot data did not contain a window governing the route's model.
    NoApplicableWindow,
    /// Equally fresh normalized snapshots disagreed about the candidate's usable evidence.
    ConflictingUsageSnapshots,
    /// Provider reported zero remaining budget.
    Exhausted,
    /// Remaining budget is at or below reserve for non-P0 work.
    ReserveProtected,
}

impl CandidateRejectionV1 {
    const fn usage_is_unknown(self) -> bool {
        matches!(
            self,
            Self::MissingUsageSnapshot
                | Self::StaleUsageSnapshot
                | Self::UsageClockSkew
                | Self::ResetElapsed
                | Self::UnhealthyUsage
                | Self::NoApplicableWindow
        )
    }
}

/// Eligibility result for one candidate.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum CandidateDispositionV1 {
    /// Candidate passed all hard constraints and has a score.
    Eligible,
    /// Candidate did not reach scoring or had no usable budget.
    Rejected {
        /// Stable fail-closed reason.
        reason: CandidateRejectionV1,
    },
}

/// Inspectable integer inputs to the weighted score.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScoreComponentsV1 {
    /// Precomputed task fit.
    pub task_fit_bps: BasisPointsV1,
    /// Remaining budget after applying reserve entitlement.
    pub usable_headroom_bps: BasisPointsV1,
    /// Least favorable reset relief across governing windows, where a larger value is better.
    pub reset_proximity_bps: BasisPointsV1,
    /// Adapter-normalized recent capacity across governing windows.
    pub recent_capacity_bps: BasisPointsV1,
    /// Precomputed latency score, where a larger value is better.
    pub latency_bps: BasisPointsV1,
    /// Confidence of the normalized usage observation.
    pub health_bps: BasisPointsV1,
    /// Explicit configured preference.
    pub configured_preference_bps: BasisPointsV1,
}

/// Deterministic evaluation record for one candidate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateEvaluationV1 {
    /// Complete candidate metadata needed to audit both winners and losers.
    pub candidate: AdaptiveCandidateV1,
    /// Eligibility or first rejection according to constraint precedence.
    pub disposition: CandidateDispositionV1,
    /// Every provider/account snapshot considered for this candidate.
    pub considered_snapshot_ids: BTreeSet<SnapshotIdV1>,
    /// Snapshot evidence that directly governed the disposition. A conflict retains every member
    /// of the ambiguous equal-time group.
    pub governing_snapshot_ids: BTreeSet<SnapshotIdV1>,
    /// Bounded adapter diagnostic retained for an unhealthy usage rejection.
    pub usage_reason_code: Option<UsageReasonCodeV1>,
    /// Provider-reported remaining budget before reserve policy.
    pub reported_remaining_bps: Option<BasisPointsV1>,
    /// Reserve applied for this provider. P0 is entitled to consume it, but it remains audited.
    pub applied_reserve_bps: Option<BasisPointsV1>,
    /// Budget remaining after reserve protection. P0 receives all reported remaining budget.
    pub usable_headroom_bps: Option<BasisPointsV1>,
    /// Every inspectable integer component used by the score.
    pub score_components: Option<ScoreComponentsV1>,
    /// Final normalized score. Rejected candidates never receive a score.
    pub score_bps: Option<BasisPointsV1>,
}

/// Authority applied to the final recommendation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppliedAuthorityV1 {
    /// A fixed authority bypassed adaptive evaluation.
    Fixed {
        /// Combined sources used by the existing resolver to compose the exact route.
        #[serde(deserialize_with = "deserialize_unique_fixed_provenances")]
        provenances: BTreeSet<FixedAuthorityProvenanceV1>,
    },
    /// Adaptive policy was evaluated.
    Adaptive,
}

/// Stable explanation for recommendation selection.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SwitchReasonV1 {
    /// A fixed authority selected the route exactly.
    FixedAuthority,
    /// No incumbent existed and the highest-scoring candidate won.
    BestCandidate,
    /// The incumbent was already the stable highest-scoring candidate.
    IncumbentBest,
    /// A challenger did not meet the inclusive switch margin.
    IncumbentHeldByHysteresis,
    /// A challenger met or exceeded the inclusive switch margin.
    ChallengerMetMargin,
    /// The incumbent was rejected and the best eligible candidate replaced it.
    IncumbentIneligible {
        /// Exact rejection that denied hysteresis protection.
        rejection: CandidateRejectionV1,
    },
    /// No route was scorable, so explicit unknown-usage policy retained the incumbent.
    IncumbentHeldOnUnknownUsage {
        /// Exact unknown-usage rejection that authorized the configured hold.
        rejection: CandidateRejectionV1,
    },
    /// No route was scorable, so the exact canonical legacy candidate was used only because its
    /// rejection represented unknown usage rather than a known hard or capacity constraint.
    LegacyFallbackOnUnknownUsage,
    /// No adaptive candidate was eligible and adaptive enforcement deferred dispatch.
    NoEligibleCandidate,
}

/// Stable reason that adaptive enforcement intentionally produced no executable route.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteDeferralReasonV1 {
    /// Adaptive mode was enabled without any configured candidate.
    NoAdaptiveCandidate,
    /// Every candidate failed authorization, capability, or quality requirements.
    HardConstraint,
    /// Every otherwise relevant candidate had known exhaustion or reserve protection.
    CapacityUnavailable,
    /// Usage was unknown and no canonical fallback candidate was safe to dispatch.
    UnknownUsage,
    /// Equally fresh normalized usage evidence conflicted, so no fallback was permitted.
    ConflictingUsageEvidence,
    /// Candidate failures spanned more than one fail-closed category.
    MixedIneligibility,
}

/// Typed recommendation or dispatch result. Deferral is not encoded as a fabricated route.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RouteOutcomeV1 {
    /// An exact route may be dispatched.
    Route {
        /// Canonical executable route.
        route: RouteTargetV1,
    },
    /// Adaptive enforcement intentionally produced no route.
    Deferred {
        /// Stable fail-closed reason.
        reason: RouteDeferralReasonV1,
    },
}

impl RouteOutcomeV1 {
    /// Returns the executable route when this outcome permits dispatch.
    #[must_use]
    pub const fn route(&self) -> Option<&RouteTargetV1> {
        match self {
            Self::Route { route } => Some(route),
            Self::Deferred { .. } => None,
        }
    }

    fn validate(&self) -> Result<(), AdaptiveRoutingError> {
        match self {
            Self::Route { route } => route.validate(),
            Self::Deferred { .. } => Ok(()),
        }
    }
}

/// Whether a previously created runtime session may be resumed for the dispatch route.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationDispositionV1 {
    /// No route is dispatched, so no session may be resumed.
    NoDispatch,
    /// No existing session runtime was supplied.
    NoSession,
    /// Dispatch stays within the same runtime family; an outer layer may attempt resume.
    SameRuntimeMayResume,
    /// Dispatch crosses runtime families; an outer layer must create a fresh session.
    FreshSessionRequired,
}

/// Routing mode actually applied to a decision.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutingDecisionModeV1 {
    /// Fixed authority bypassed adaptive policy entirely.
    Fixed,
    /// Adaptive policy was evaluated under the recorded evaluation mode.
    Adaptive {
        /// Whether the recommendation controlled dispatch or was shadow-only.
        evaluation_mode: AdaptiveEvaluationModeV1,
    },
}

/// Version-one deterministic routing decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingDecisionV1 {
    /// Decision schema version.
    pub schema_version: u32,
    /// Canonical policy used for adaptive evaluation; fixed authority records `None`.
    pub applied_policy: Option<AdaptivePolicyV1>,
    /// Caller-supplied evaluation time.
    pub evaluated_at_utc_ms: UtcMillisV1,
    /// Mission identity copied into the decision for standalone audit correlation.
    pub mission_id: MissionId,
    /// Phase identity copied into the decision for standalone audit correlation.
    pub phase_id: PhaseId,
    /// One-based attempt copied into the decision for standalone audit correlation.
    pub attempt: u32,
    /// Applied fixed or adaptive authority.
    pub applied_authority: AppliedAuthorityV1,
    /// Hard requirements used for candidate evaluation.
    pub requirements: RouteRequirementsV1,
    /// Incumbent identity supplied to hysteresis and unknown-usage handling.
    pub incumbent_candidate_id: Option<CandidateIdV1>,
    /// Existing runtime supplied to tie-breaking and continuation derivation.
    pub existing_session_runtime: Option<RuntimeIdV1>,
    /// Exact legacy route supplied for adaptive shadow dispatch and canonical unknown-usage
    /// fallback. Fixed authority records `None` because the legacy route was not evaluated.
    pub legacy_route: Option<RouteTargetV1>,
    /// Stable candidate evaluations sorted by candidate identifier.
    pub candidate_evaluations: Vec<CandidateEvaluationV1>,
    /// Stable snapshots considered by candidate evaluations, sorted by snapshot identifier.
    pub referenced_snapshots: Vec<UsageSnapshotV1>,
    /// Route or deferral recommended by fixed authority or adaptive policy.
    pub recommended_outcome: RouteOutcomeV1,
    /// Route or deferral the caller must apply. In shadow mode this remains a legacy route.
    pub dispatch_outcome: RouteOutcomeV1,
    /// Stable selection reason.
    pub switch_reason: SwitchReasonV1,
    /// Session disposition derived only from runtime family, never session identity.
    pub continuation: ContinuationDispositionV1,
    /// Fixed or adaptive mode actually applied to this decision.
    pub decision_mode: RoutingDecisionModeV1,
}

impl RoutingDecisionV1 {
    fn validate_replay_shape(&self) -> Result<(), AdaptiveRoutingError> {
        validate_schema("routing decision", self.schema_version)?;
        if self.attempt == 0 {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "routing_decision.attempt",
                reason: "attempt is one-based",
            });
        }
        if self.candidate_evaluations.len() > MAX_CANDIDATES {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "routing_decision.candidate_evaluations",
                reason: "too many candidate evaluations",
            });
        }
        if self.referenced_snapshots.len() > MAX_USAGE_SNAPSHOTS {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "routing_decision.referenced_snapshots",
                reason: "too many referenced snapshots",
            });
        }
        if self.requirements.required_capabilities.len() > MAX_CAPABILITIES {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "routing_decision.requirements.required_capabilities",
                reason: "too many required capabilities",
            });
        }
        if let Some(legacy_route) = &self.legacy_route {
            legacy_route.validate()?;
        }
        self.recommended_outcome.validate()?;
        self.dispatch_outcome.validate()?;
        if continuation_for(
            self.existing_session_runtime.as_ref(),
            &self.dispatch_outcome,
        ) != self.continuation
        {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "routing_decision.continuation",
                reason: "continuation does not match its recorded input and dispatch outcome",
            });
        }

        let mut budget = RoutingBudget::default();
        budget.add_text(self.mission_id.as_str())?;
        budget.add_text(self.phase_id.as_str())?;
        budget.add_elements(self.requirements.required_capabilities.len())?;
        for capability in &self.requirements.required_capabilities {
            budget.add_text(capability.as_str())?;
        }
        if let Some(incumbent) = &self.incumbent_candidate_id {
            budget.add_text(incumbent.as_str())?;
        }
        if let Some(runtime) = &self.existing_session_runtime {
            budget.add_text(runtime.as_str())?;
        }
        if let Some(legacy_route) = &self.legacy_route {
            account_route_budget(&mut budget, legacy_route)?;
        }

        let mut referenced_snapshot_ids = BTreeSet::new();
        let mut previous_snapshot_id = None;
        for snapshot in &self.referenced_snapshots {
            snapshot.validate()?;
            if snapshot
                .windows
                .windows(2)
                .any(|windows| windows[0].limit_id.as_str() >= windows[1].limit_id.as_str())
            {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_decision.referenced_snapshots.windows",
                    reason: "referenced snapshot windows must be sorted by limit identifier",
                });
            }
            if previous_snapshot_id.is_some_and(|previous| previous >= &snapshot.snapshot_id) {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_decision.referenced_snapshots",
                    reason: "referenced snapshots must be uniquely sorted by identifier",
                });
            }
            previous_snapshot_id = Some(&snapshot.snapshot_id);
            if !referenced_snapshot_ids.insert(snapshot.snapshot_id.clone()) {
                return Err(AdaptiveRoutingError::DuplicateSnapshot);
            }
            account_snapshot_budget(&mut budget, snapshot)?;
        }

        let mut candidate_ids = BTreeSet::new();
        let mut considered_snapshot_ids = BTreeSet::new();
        let mut previous_candidate_id = None;
        let mut considered_snapshot_references = 0_usize;
        let mut usage_window_evaluations = 0_usize;
        for evaluation in &self.candidate_evaluations {
            evaluation.candidate.route.validate()?;
            if evaluation.candidate.capabilities.len() > MAX_CAPABILITIES {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_decision.candidate.capabilities",
                    reason: "too many candidate capabilities",
                });
            }
            let candidate_id = &evaluation.candidate.route.candidate_id;
            if previous_candidate_id.is_some_and(|previous| previous >= candidate_id) {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_decision.candidate_evaluations",
                    reason: "candidate evaluations must be uniquely sorted by identifier",
                });
            }
            previous_candidate_id = Some(candidate_id);
            if !candidate_ids.insert(candidate_id.clone()) {
                return Err(AdaptiveRoutingError::DuplicateCandidate);
            }
            account_candidate_budget(&mut budget, &evaluation.candidate)?;
            if !evaluation
                .governing_snapshot_ids
                .is_subset(&evaluation.considered_snapshot_ids)
            {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_decision.candidate_evaluations",
                    reason: "governing snapshots must be a subset of considered snapshots",
                });
            }
            for snapshot_id in &evaluation.considered_snapshot_ids {
                considered_snapshot_references = considered_snapshot_references
                    .checked_add(1)
                    .ok_or(AdaptiveRoutingError::InvalidInvariant {
                        field: "adaptive_routing.considered_snapshot_references",
                        reason: "considered snapshot-reference count overflowed",
                    })?;
                if considered_snapshot_references > MAX_TOTAL_CONSIDERED_SNAPSHOT_REFERENCES {
                    return Err(AdaptiveRoutingError::InvalidInvariant {
                        field: "adaptive_routing.considered_snapshot_references",
                        reason: "too many considered snapshot references",
                    });
                }
                considered_snapshot_ids.insert(snapshot_id.clone());
                let snapshot = self
                    .referenced_snapshots
                    .iter()
                    .find(|snapshot| &snapshot.snapshot_id == snapshot_id)
                    .ok_or(AdaptiveRoutingError::InvalidInvariant {
                        field: "routing_decision.referenced_snapshots",
                        reason: "considered snapshot evidence is missing",
                    })?;
                usage_window_evaluations = usage_window_evaluations
                    .checked_add(snapshot.windows.len())
                    .ok_or(AdaptiveRoutingError::InvalidInvariant {
                        field: "adaptive_routing.usage_window_evaluations",
                        reason: "usage evaluation work count overflowed",
                    })?;
                if usage_window_evaluations > MAX_USAGE_WINDOW_EVALUATIONS {
                    return Err(AdaptiveRoutingError::InvalidInvariant {
                        field: "adaptive_routing.usage_window_evaluations",
                        reason: "too much aggregate usage evaluation work",
                    });
                }
            }
            let eligible_shape = evaluation.score_bps.is_some()
                && evaluation.score_components.is_some()
                && !evaluation.governing_snapshot_ids.is_empty()
                && evaluation.usage_reason_code.is_none()
                && evaluation.reported_remaining_bps.is_some()
                && evaluation.applied_reserve_bps.is_some()
                && evaluation.usable_headroom_bps.is_some();
            match evaluation.disposition {
                CandidateDispositionV1::Eligible if !eligible_shape => {
                    return Err(AdaptiveRoutingError::InvalidInvariant {
                        field: "routing_decision.candidate_evaluations",
                        reason: "eligible evaluation is missing audit components",
                    });
                }
                CandidateDispositionV1::Rejected { reason }
                    if evaluation.score_bps.is_some()
                        || evaluation.score_components.is_some()
                        || evaluation.usable_headroom_bps.is_some()
                        || (reason == CandidateRejectionV1::UnhealthyUsage)
                            != evaluation.usage_reason_code.is_some()
                        || (reason == CandidateRejectionV1::ConflictingUsageSnapshots
                            && evaluation.governing_snapshot_ids.len() < 2) =>
                {
                    return Err(AdaptiveRoutingError::InvalidInvariant {
                        field: "routing_decision.candidate_evaluations",
                        reason: "rejected evaluation has an invalid score, headroom, or reason code",
                    });
                }
                CandidateDispositionV1::Eligible | CandidateDispositionV1::Rejected { .. } => {}
            }
        }
        if considered_snapshot_ids != referenced_snapshot_ids {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "routing_decision.referenced_snapshots",
                reason: "referenced snapshots do not match considered candidate evidence",
            });
        }
        if let AppliedAuthorityV1::Fixed { provenances } = &self.applied_authority {
            if self.decision_mode != RoutingDecisionModeV1::Fixed {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_decision.decision_mode",
                    reason: "fixed authority requires fixed decision mode",
                });
            }
            if self.applied_policy.is_some() {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_decision.applied_policy",
                    reason: "fixed decision cannot claim an applied adaptive policy",
                });
            }
            if self.legacy_route.is_some() {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_decision.legacy_route",
                    reason: "fixed decision cannot claim an unused legacy route",
                });
            }
            if provenances.is_empty() || provenances.len() > MAX_FIXED_PROVENANCES {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_decision.applied_authority.provenances",
                    reason: "fixed authority provenance count is invalid",
                });
            }
            if !self.candidate_evaluations.is_empty() || !self.referenced_snapshots.is_empty() {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_decision.applied_authority",
                    reason: "fixed decision cannot contain adaptive evaluations",
                });
            }
            if !matches!(&self.recommended_outcome, RouteOutcomeV1::Route { .. })
                || !matches!(&self.dispatch_outcome, RouteOutcomeV1::Route { .. })
            {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_decision.fixed_outcome",
                    reason: "fixed authority must contain executable route outcomes",
                });
            }
            if self.switch_reason != SwitchReasonV1::FixedAuthority {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_decision.switch_reason",
                    reason: "fixed authority requires the fixed-authority switch reason",
                });
            }
        } else {
            let applied_policy =
                self.applied_policy
                    .as_ref()
                    .ok_or(AdaptiveRoutingError::InvalidInvariant {
                        field: "routing_decision.applied_policy",
                        reason: "adaptive decision must retain the canonical applied policy",
                    })?;
            if self.legacy_route.is_none() {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_decision.legacy_route",
                    reason: "adaptive decision must retain the exact legacy route",
                });
            }
            applied_policy.validate()?;
            let canonical_reserves = applied_policy.canonicalized().provider_reserves;
            if applied_policy.provider_reserves.as_slice() != canonical_reserves.as_slice() {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_decision.applied_policy.provider_reserves",
                    reason: "applied provider reserves must be canonically sorted",
                });
            }
            if self.decision_mode
                != (RoutingDecisionModeV1::Adaptive {
                    evaluation_mode: applied_policy.evaluation_mode,
                })
            {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_decision.decision_mode",
                    reason: "adaptive decision mode does not match the applied policy",
                });
            }
            if self
                .incumbent_candidate_id
                .as_ref()
                .is_some_and(|incumbent| !candidate_ids.contains(incumbent))
            {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_decision.incumbent_candidate_id",
                    reason: "incumbent has no corresponding candidate evaluation",
                });
            }
            match self.switch_reason {
                SwitchReasonV1::FixedAuthority => {
                    return Err(AdaptiveRoutingError::InvalidInvariant {
                        field: "routing_decision.switch_reason",
                        reason: "adaptive authority cannot claim a fixed switch reason",
                    });
                }
                SwitchReasonV1::BestCandidate if self.incumbent_candidate_id.is_some() => {
                    return Err(AdaptiveRoutingError::InvalidInvariant {
                        field: "routing_decision.switch_reason",
                        reason: "best-candidate reason requires no incumbent",
                    });
                }
                SwitchReasonV1::IncumbentBest
                | SwitchReasonV1::IncumbentHeldByHysteresis
                | SwitchReasonV1::ChallengerMetMargin
                    if self.incumbent_candidate_id.is_none() =>
                {
                    return Err(AdaptiveRoutingError::InvalidInvariant {
                        field: "routing_decision.switch_reason",
                        reason: "incumbent comparison reason requires an incumbent",
                    });
                }
                SwitchReasonV1::IncumbentIneligible { rejection }
                | SwitchReasonV1::IncumbentHeldOnUnknownUsage { rejection } => {
                    let incumbent = self.incumbent_candidate_id.as_ref().ok_or(
                        AdaptiveRoutingError::InvalidInvariant {
                            field: "routing_decision.switch_reason",
                            reason: "typed incumbent reason requires an incumbent",
                        },
                    )?;
                    let evaluation = self
                        .candidate_evaluations
                        .iter()
                        .find(|evaluation| &evaluation.candidate.route.candidate_id == incumbent)
                        .ok_or(AdaptiveRoutingError::InvalidInvariant {
                            field: "routing_decision.switch_reason",
                            reason: "typed incumbent reason has no incumbent evaluation",
                        })?;
                    if evaluation.disposition
                        != (CandidateDispositionV1::Rejected { reason: rejection })
                    {
                        return Err(AdaptiveRoutingError::InvalidInvariant {
                            field: "routing_decision.switch_reason",
                            reason: "typed incumbent reason does not match its evaluation",
                        });
                    }
                    if matches!(
                        self.switch_reason,
                        SwitchReasonV1::IncumbentHeldOnUnknownUsage { .. }
                    ) && (!rejection.usage_is_unknown()
                        || self
                            .recommended_outcome
                            .route()
                            .map(|route| &route.candidate_id)
                            != Some(incumbent))
                    {
                        return Err(AdaptiveRoutingError::InvalidInvariant {
                            field: "routing_decision.switch_reason",
                            reason: "incumbent hold requires unknown usage and the incumbent route",
                        });
                    }
                }
                SwitchReasonV1::BestCandidate
                | SwitchReasonV1::IncumbentBest
                | SwitchReasonV1::IncumbentHeldByHysteresis
                | SwitchReasonV1::ChallengerMetMargin
                | SwitchReasonV1::LegacyFallbackOnUnknownUsage
                | SwitchReasonV1::NoEligibleCandidate => {}
            }
        }
        Ok(())
    }
}

/// Complete replay envelope. Recompute must exactly match `expected_decision`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RoutingReplayRecordV1 {
    schema_version: u32,
    policy: AdaptivePolicyV1,
    input: RoutingInputV1,
    expected_decision: RoutingDecisionV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutingReplayRecordWireV1 {
    schema_version: u32,
    policy: AdaptivePolicyV1,
    input: RoutingInputV1,
    expected_decision: RoutingDecisionV1,
}

impl RoutingReplayRecordV1 {
    /// Constructs a validated replay record and canonicalizes provider reserves.
    pub fn new(
        policy: AdaptivePolicyV1,
        input: RoutingInputV1,
        expected_decision: RoutingDecisionV1,
    ) -> Result<Self, AdaptiveRoutingError> {
        policy.validate_provider_reserves()?;
        let record = Self {
            schema_version: ADAPTIVE_ROUTING_SCHEMA_V1,
            policy: policy.canonicalized(),
            input,
            expected_decision,
        };
        record.recompute_and_validate()?;
        Ok(record)
    }

    /// Returns the replay envelope schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the canonical source policy.
    #[must_use]
    pub const fn policy(&self) -> &AdaptivePolicyV1 {
        &self.policy
    }

    /// Returns the exact normalized source input.
    #[must_use]
    pub const fn input(&self) -> &RoutingInputV1 {
        &self.input
    }

    /// Returns the deterministic decision expected from replay.
    #[must_use]
    pub const fn expected_decision(&self) -> &RoutingDecisionV1 {
        &self.expected_decision
    }

    fn recompute_and_validate(&self) -> Result<RoutingDecisionV1, AdaptiveRoutingError> {
        validate_schema("routing replay", self.schema_version)?;
        self.policy.validate_provider_reserves()?;
        let canonical_reserves = self.policy.canonicalized().provider_reserves;
        if self.policy.provider_reserves.as_slice() != canonical_reserves.as_slice() {
            return Err(AdaptiveRoutingError::InvalidInvariant {
                field: "routing_replay.policy.provider_reserves",
                reason: "source provider reserves must be canonically sorted",
            });
        }
        self.expected_decision.validate_replay_shape()?;
        let decision = decide_route(&self.policy, &self.input)?;
        if decision != self.expected_decision {
            return Err(AdaptiveRoutingError::ReplayMismatch);
        }
        Ok(decision)
    }
}

impl<'de> Deserialize<'de> for RoutingReplayRecordV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RoutingReplayRecordWireV1::deserialize(deserializer)?;
        validate_schema("routing replay", wire.schema_version).map_err(de::Error::custom)?;
        wire.policy
            .validate_provider_reserves()
            .map_err(de::Error::custom)?;
        let canonical_reserves = wire.policy.canonicalized().provider_reserves;
        if wire.policy.provider_reserves.as_slice() != canonical_reserves.as_slice() {
            return Err(de::Error::custom(
                "source provider reserves must be canonically sorted",
            ));
        }
        let record = Self {
            schema_version: wire.schema_version,
            policy: wire.policy,
            input: wire.input,
            expected_decision: wire.expected_decision,
        };
        record.recompute_and_validate().map_err(de::Error::custom)?;
        Ok(record)
    }
}

#[derive(Clone, Copy)]
struct UsageAssessment {
    remaining_bps: BasisPointsV1,
    reset_proximity_bps: BasisPointsV1,
    recent_capacity_bps: BasisPointsV1,
    health_bps: BasisPointsV1,
}

enum UsageProbe<'a> {
    Usable {
        assessment: UsageAssessment,
        considered_snapshot_ids: BTreeSet<SnapshotIdV1>,
        governing_snapshot_ids: BTreeSet<SnapshotIdV1>,
    },
    Rejected {
        reason: CandidateRejectionV1,
        considered_snapshot_ids: BTreeSet<SnapshotIdV1>,
        governing_snapshot_ids: BTreeSet<SnapshotIdV1>,
        reason_code: Option<&'a UsageReasonCodeV1>,
    },
}

fn validate_schema(surface: &'static str, found: u32) -> Result<(), AdaptiveRoutingError> {
    if found == ADAPTIVE_ROUTING_SCHEMA_V1 {
        Ok(())
    } else {
        Err(AdaptiveRoutingError::UnsupportedSchemaVersion {
            surface,
            found,
            expected: ADAPTIVE_ROUTING_SCHEMA_V1,
        })
    }
}

fn continuation_for(
    existing_session_runtime: Option<&RuntimeIdV1>,
    dispatch_outcome: &RouteOutcomeV1,
) -> ContinuationDispositionV1 {
    let Some(dispatch_runtime) = dispatch_outcome.route().map(|route| &route.runtime) else {
        return ContinuationDispositionV1::NoDispatch;
    };
    match existing_session_runtime {
        None => ContinuationDispositionV1::NoSession,
        Some(existing) if existing == dispatch_runtime => {
            ContinuationDispositionV1::SameRuntimeMayResume
        }
        Some(_) => ContinuationDispositionV1::FreshSessionRequired,
    }
}

fn reject(
    candidate: &AdaptiveCandidateV1,
    reason: CandidateRejectionV1,
    considered_snapshot_ids: BTreeSet<SnapshotIdV1>,
    governing_snapshot_ids: BTreeSet<SnapshotIdV1>,
    usage_reason_code: Option<UsageReasonCodeV1>,
) -> CandidateEvaluationV1 {
    CandidateEvaluationV1 {
        candidate: candidate.clone(),
        disposition: CandidateDispositionV1::Rejected { reason },
        considered_snapshot_ids,
        governing_snapshot_ids,
        usage_reason_code,
        reported_remaining_bps: None,
        applied_reserve_bps: None,
        usable_headroom_bps: None,
        score_components: None,
        score_bps: None,
    }
}

fn reject_with_budget(
    candidate: &AdaptiveCandidateV1,
    reason: CandidateRejectionV1,
    considered_snapshot_ids: BTreeSet<SnapshotIdV1>,
    governing_snapshot_ids: BTreeSet<SnapshotIdV1>,
    reported_remaining_bps: BasisPointsV1,
    applied_reserve_bps: BasisPointsV1,
) -> CandidateEvaluationV1 {
    CandidateEvaluationV1 {
        candidate: candidate.clone(),
        disposition: CandidateDispositionV1::Rejected { reason },
        considered_snapshot_ids,
        governing_snapshot_ids,
        usage_reason_code: None,
        reported_remaining_bps: Some(reported_remaining_bps),
        applied_reserve_bps: Some(applied_reserve_bps),
        usable_headroom_bps: None,
        score_components: None,
        score_bps: None,
    }
}

fn snapshot_assessment(
    snapshot: &UsageSnapshotV1,
    model: &str,
    evaluated_at: UtcMillisV1,
    maximum_age: DurationMillisV1,
) -> Result<UsageAssessment, CandidateRejectionV1> {
    if evaluated_at < snapshot.received_at_utc_ms {
        return Err(CandidateRejectionV1::UsageClockSkew);
    }
    let age = evaluated_at.get() - snapshot.observed_at_utc_ms.get();
    let accepted_age = snapshot.ttl_ms.get().min(maximum_age.get());
    if age >= accepted_age {
        return Err(CandidateRejectionV1::StaleUsageSnapshot);
    }
    if snapshot.health != UsageHealthV1::Healthy {
        return Err(CandidateRejectionV1::UnhealthyUsage);
    }

    let applicable = snapshot.windows.iter().filter(|window| {
        window.applicable_models.is_empty() || window.applicable_models.contains(model)
    });
    let mut found = false;
    let mut remaining = BASIS_POINTS_SCALE;
    let mut reset_proximity = BASIS_POINTS_SCALE;
    let mut recent_capacity = BASIS_POINTS_SCALE;
    for window in applicable {
        found = true;
        if window.resets_at_utc_ms <= evaluated_at {
            return Err(CandidateRejectionV1::ResetElapsed);
        }
        remaining = remaining.min(window.remaining_bps.get());
        recent_capacity = recent_capacity.min(window.recent_capacity_bps.get());
        let horizon_ms = window.resets_at_utc_ms.get() - evaluated_at.get();
        let horizon_ratio = horizon_ms * u64::from(BASIS_POINTS_SCALE) / window.duration_ms.get();
        let horizon_ratio = u16::try_from(horizon_ratio.min(u64::from(BASIS_POINTS_SCALE)))
            .map_err(|_| CandidateRejectionV1::UnhealthyUsage)?;
        reset_proximity = reset_proximity.min(BASIS_POINTS_SCALE - horizon_ratio);
    }
    if !found {
        return Err(CandidateRejectionV1::NoApplicableWindow);
    }

    let remaining_bps =
        BasisPointsV1::new(remaining).map_err(|_| CandidateRejectionV1::UnhealthyUsage)?;
    let reset_proximity_bps =
        BasisPointsV1::new(reset_proximity).map_err(|_| CandidateRejectionV1::UnhealthyUsage)?;
    let recent_capacity_bps =
        BasisPointsV1::new(recent_capacity).map_err(|_| CandidateRejectionV1::UnhealthyUsage)?;
    Ok(UsageAssessment {
        remaining_bps,
        reset_proximity_bps,
        recent_capacity_bps,
        health_bps: snapshot.confidence_bps,
    })
}

fn usage_for_candidate<'a>(
    candidate: &AdaptiveCandidateV1,
    snapshots: &'a [UsageSnapshotV1],
    evaluated_at: UtcMillisV1,
    maximum_age: DurationMillisV1,
) -> UsageProbe<'a> {
    fn assessments_are_equivalent(
        left_snapshot: &UsageSnapshotV1,
        left: &Result<UsageAssessment, CandidateRejectionV1>,
        right_snapshot: &UsageSnapshotV1,
        right: &Result<UsageAssessment, CandidateRejectionV1>,
    ) -> bool {
        match (left, right) {
            (Ok(left), Ok(right)) => {
                left.remaining_bps == right.remaining_bps
                    && left.reset_proximity_bps == right.reset_proximity_bps
                    && left.recent_capacity_bps == right.recent_capacity_bps
                    && left.health_bps == right.health_bps
            }
            (Err(left), Err(right)) => {
                left == right
                    && (*left != CandidateRejectionV1::UnhealthyUsage
                        || left_snapshot.reason_code == right_snapshot.reason_code)
            }
            (Ok(_), Err(_)) | (Err(_), Ok(_)) => false,
        }
    }

    let mut ordered = snapshots
        .iter()
        .filter(|snapshot| {
            snapshot.provider == candidate.route.provider
                && snapshot.usage_account_id == candidate.route.usage_account_id
        })
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        right
            .observed_at_utc_ms
            .cmp(&left.observed_at_utc_ms)
            .then_with(|| right.received_at_utc_ms.cmp(&left.received_at_utc_ms))
            .then_with(|| left.snapshot_id.cmp(&right.snapshot_id))
    });
    let considered_snapshot_ids = ordered
        .iter()
        .map(|snapshot| snapshot.snapshot_id.clone())
        .collect::<BTreeSet<_>>();

    let mut newest_rejection = None;
    let mut remaining = ordered.as_slice();
    while let Some((first, tail)) = remaining.split_first() {
        let first = *first;
        let additional_group_members = tail
            .iter()
            .take_while(|snapshot| {
                snapshot.observed_at_utc_ms == first.observed_at_utc_ms
                    && snapshot.received_at_utc_ms == first.received_at_utc_ms
            })
            .count();
        let group_len = additional_group_members + 1;
        let (group, rest) = remaining.split_at(group_len);
        let governing_snapshot_ids = group
            .iter()
            .map(|snapshot| snapshot.snapshot_id.clone())
            .collect::<BTreeSet<_>>();
        let first_assessment =
            snapshot_assessment(first, &candidate.route.model, evaluated_at, maximum_age);
        let conflict = group.iter().skip(1).any(|snapshot| {
            let snapshot = *snapshot;
            let assessment =
                snapshot_assessment(snapshot, &candidate.route.model, evaluated_at, maximum_age);
            !assessments_are_equivalent(first, &first_assessment, snapshot, &assessment)
        });
        if conflict {
            return UsageProbe::Rejected {
                reason: CandidateRejectionV1::ConflictingUsageSnapshots,
                considered_snapshot_ids,
                governing_snapshot_ids,
                reason_code: None,
            };
        }
        match first_assessment {
            Ok(assessment) => {
                return UsageProbe::Usable {
                    assessment,
                    considered_snapshot_ids,
                    governing_snapshot_ids,
                };
            }
            Err(reason) if newest_rejection.is_none() => {
                newest_rejection = Some((first, reason, governing_snapshot_ids));
            }
            Err(_) => {}
        }
        remaining = rest;
    }
    match newest_rejection {
        Some((snapshot, reason, governing_snapshot_ids)) => UsageProbe::Rejected {
            reason,
            considered_snapshot_ids,
            governing_snapshot_ids,
            reason_code: if reason == CandidateRejectionV1::UnhealthyUsage {
                snapshot.reason_code.as_ref()
            } else {
                None
            },
        },
        None => UsageProbe::Rejected {
            reason: CandidateRejectionV1::MissingUsageSnapshot,
            considered_snapshot_ids,
            governing_snapshot_ids: BTreeSet::new(),
            reason_code: None,
        },
    }
}

fn weighted_score(
    policy: &AdaptivePolicyV1,
    components: ScoreComponentsV1,
) -> Result<BasisPointsV1, AdaptiveRoutingError> {
    let weighted_components = [
        (components.task_fit_bps, policy.score_weights.task_fit_bps),
        (
            components.usable_headroom_bps,
            policy.score_weights.usable_headroom_bps,
        ),
        (
            components.reset_proximity_bps,
            policy.score_weights.reset_proximity_bps,
        ),
        (
            components.recent_capacity_bps,
            policy.score_weights.recent_capacity_bps,
        ),
        (components.latency_bps, policy.score_weights.latency_bps),
        (components.health_bps, policy.score_weights.health_bps),
        (
            components.configured_preference_bps,
            policy.score_weights.configured_preference_bps,
        ),
    ];
    let mut weighted_total = 0_u64;
    for (component, weight) in weighted_components {
        let term = u64::from(component.get())
            .checked_mul(u64::from(weight.get()))
            .ok_or(AdaptiveRoutingError::ScoreOverflow)?;
        weighted_total = weighted_total
            .checked_add(term)
            .ok_or(AdaptiveRoutingError::ScoreOverflow)?;
    }
    let normalized = weighted_total / u64::from(BASIS_POINTS_SCALE);
    let normalized = u16::try_from(normalized).map_err(|_| AdaptiveRoutingError::ScoreOverflow)?;
    BasisPointsV1::new(normalized)
}

fn evaluate_candidate(
    policy: &AdaptivePolicyV1,
    input: &RoutingInputV1,
    candidate: &AdaptiveCandidateV1,
) -> Result<CandidateEvaluationV1, AdaptiveRoutingError> {
    match candidate.automatic_eligibility {
        AutomaticEligibilityV1::AdvisoryOnly => {
            return Ok(reject(
                candidate,
                CandidateRejectionV1::AdvisoryOnly,
                BTreeSet::new(),
                BTreeSet::new(),
                None,
            ));
        }
        AutomaticEligibilityV1::ExplicitOnly => {
            return Ok(reject(
                candidate,
                CandidateRejectionV1::ExplicitOnly,
                BTreeSet::new(),
                BTreeSet::new(),
                None,
            ));
        }
        AutomaticEligibilityV1::Automatic => {}
    }
    if !input
        .requirements
        .required_capabilities
        .is_subset(&candidate.capabilities)
    {
        return Ok(reject(
            candidate,
            CandidateRejectionV1::MissingCapability,
            BTreeSet::new(),
            BTreeSet::new(),
            None,
        ));
    }
    if candidate.quality.rank() < input.requirements.minimum_quality.rank() {
        return Ok(reject(
            candidate,
            CandidateRejectionV1::BelowQualityFloor,
            BTreeSet::new(),
            BTreeSet::new(),
            None,
        ));
    }

    let (assessment, considered_snapshot_ids, governing_snapshot_ids) = match usage_for_candidate(
        candidate,
        &input.usage_snapshots,
        input.evaluated_at_utc_ms,
        policy.maximum_snapshot_age_ms,
    ) {
        UsageProbe::Usable {
            assessment,
            considered_snapshot_ids,
            governing_snapshot_ids,
        } => (assessment, considered_snapshot_ids, governing_snapshot_ids),
        UsageProbe::Rejected {
            reason,
            considered_snapshot_ids,
            governing_snapshot_ids,
            reason_code,
        } => {
            return Ok(reject(
                candidate,
                reason,
                considered_snapshot_ids,
                governing_snapshot_ids,
                reason_code.cloned(),
            ));
        }
    };
    let remaining = assessment.remaining_bps.get();
    let reserve = policy
        .reserve_for(&candidate.route.provider, &candidate.route.usage_account_id)
        .ok_or(AdaptiveRoutingError::MissingProviderReserve)?;
    if remaining == 0 {
        return Ok(reject_with_budget(
            candidate,
            CandidateRejectionV1::Exhausted,
            considered_snapshot_ids,
            governing_snapshot_ids,
            assessment.remaining_bps,
            reserve,
        ));
    }
    let usable = if input.requirements.priority == TaskPriorityV1::P0 {
        assessment.remaining_bps
    } else if remaining <= reserve.get() {
        return Ok(reject_with_budget(
            candidate,
            CandidateRejectionV1::ReserveProtected,
            considered_snapshot_ids,
            governing_snapshot_ids,
            assessment.remaining_bps,
            reserve,
        ));
    } else {
        BasisPointsV1::new(remaining - reserve.get())?
    };
    let score_components = ScoreComponentsV1 {
        task_fit_bps: candidate.task_fit_bps,
        usable_headroom_bps: usable,
        reset_proximity_bps: assessment.reset_proximity_bps,
        recent_capacity_bps: assessment.recent_capacity_bps,
        latency_bps: candidate.latency_bps,
        health_bps: assessment.health_bps,
        configured_preference_bps: candidate.configured_preference_bps,
    };
    let score = weighted_score(policy, score_components)?;
    Ok(CandidateEvaluationV1 {
        candidate: candidate.clone(),
        disposition: CandidateDispositionV1::Eligible,
        considered_snapshot_ids,
        governing_snapshot_ids,
        usage_reason_code: None,
        reported_remaining_bps: Some(assessment.remaining_bps),
        applied_reserve_bps: Some(reserve),
        usable_headroom_bps: Some(usable),
        score_components: Some(score_components),
        score_bps: Some(score),
    })
}

fn eligible_score(evaluation: &CandidateEvaluationV1) -> Option<BasisPointsV1> {
    if evaluation.disposition == CandidateDispositionV1::Eligible {
        evaluation.score_bps
    } else {
        None
    }
}

fn preserves_existing_runtime(input: &RoutingInputV1, evaluation: &CandidateEvaluationV1) -> bool {
    let Some(existing_runtime) = &input.existing_session_runtime else {
        return false;
    };
    &evaluation.candidate.route.runtime == existing_runtime
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RejectionClass {
    HardConstraint,
    UnknownUsage,
    CapacityUnavailable,
    ConflictingUsageEvidence,
}

const fn rejection_class(reason: CandidateRejectionV1) -> RejectionClass {
    match reason {
        CandidateRejectionV1::AdvisoryOnly
        | CandidateRejectionV1::ExplicitOnly
        | CandidateRejectionV1::MissingCapability
        | CandidateRejectionV1::BelowQualityFloor => RejectionClass::HardConstraint,
        CandidateRejectionV1::MissingUsageSnapshot
        | CandidateRejectionV1::StaleUsageSnapshot
        | CandidateRejectionV1::UsageClockSkew
        | CandidateRejectionV1::ResetElapsed
        | CandidateRejectionV1::UnhealthyUsage
        | CandidateRejectionV1::NoApplicableWindow => RejectionClass::UnknownUsage,
        CandidateRejectionV1::ConflictingUsageSnapshots => RejectionClass::ConflictingUsageEvidence,
        CandidateRejectionV1::Exhausted | CandidateRejectionV1::ReserveProtected => {
            RejectionClass::CapacityUnavailable
        }
    }
}

fn deferral_reason(evaluations: &[CandidateEvaluationV1]) -> RouteDeferralReasonV1 {
    let mut class = None;
    for evaluation in evaluations {
        let CandidateDispositionV1::Rejected { reason } = evaluation.disposition else {
            continue;
        };
        let next = rejection_class(reason);
        match class {
            None => class = Some(next),
            Some(current) if current == next => {}
            Some(_) => return RouteDeferralReasonV1::MixedIneligibility,
        }
    }
    match class {
        None => RouteDeferralReasonV1::NoAdaptiveCandidate,
        Some(RejectionClass::HardConstraint) => RouteDeferralReasonV1::HardConstraint,
        Some(RejectionClass::UnknownUsage) => RouteDeferralReasonV1::UnknownUsage,
        Some(RejectionClass::CapacityUnavailable) => RouteDeferralReasonV1::CapacityUnavailable,
        Some(RejectionClass::ConflictingUsageEvidence) => {
            RouteDeferralReasonV1::ConflictingUsageEvidence
        }
    }
}

fn canonical_legacy_candidate(input: &RoutingInputV1) -> Option<&AdaptiveCandidateV1> {
    input.candidates.iter().find(|candidate| {
        candidate
            .route
            .same_executable_target(&input.authority.legacy_route)
    })
}

fn select_recommendation(
    policy: &AdaptivePolicyV1,
    input: &RoutingInputV1,
    evaluations: &[CandidateEvaluationV1],
) -> Result<(RouteOutcomeV1, SwitchReasonV1), AdaptiveRoutingError> {
    let mut best: Option<&CandidateEvaluationV1> = None;
    for evaluation in evaluations {
        let Some(score) = eligible_score(evaluation) else {
            continue;
        };
        let replace = match best {
            None => true,
            Some(current) => {
                let current_score =
                    eligible_score(current).ok_or(AdaptiveRoutingError::ScoreOverflow)?;
                score > current_score
                    || (score == current_score
                        && preserves_existing_runtime(input, evaluation)
                        && !preserves_existing_runtime(input, current))
            }
        };
        if replace {
            best = Some(evaluation);
        }
    }

    let incumbent_evaluation = input.incumbent_candidate_id.as_ref().and_then(|incumbent| {
        evaluations
            .iter()
            .find(|evaluation| &evaluation.candidate.route.candidate_id == incumbent)
    });

    let selected = match (best, incumbent_evaluation) {
        (Some(best), Some(incumbent)) if eligible_score(incumbent).is_some() => {
            if best.candidate.route.candidate_id == incumbent.candidate.route.candidate_id {
                (incumbent, SwitchReasonV1::IncumbentBest)
            } else {
                let best_score = eligible_score(best).ok_or(AdaptiveRoutingError::ScoreOverflow)?;
                let incumbent_score =
                    eligible_score(incumbent).ok_or(AdaptiveRoutingError::ScoreOverflow)?;
                let threshold = u32::from(incumbent_score.get())
                    .checked_add(u32::from(policy.switch_margin_bps.get()))
                    .ok_or(AdaptiveRoutingError::ScoreOverflow)?;
                if u32::from(best_score.get()) >= threshold {
                    (best, SwitchReasonV1::ChallengerMetMargin)
                } else {
                    (incumbent, SwitchReasonV1::IncumbentHeldByHysteresis)
                }
            }
        }
        (Some(best), Some(incumbent)) => {
            let CandidateDispositionV1::Rejected { reason } = incumbent.disposition else {
                return Err(AdaptiveRoutingError::InvalidInvariant {
                    field: "routing_input.incumbent_candidate_id",
                    reason: "incumbent evaluation has no eligible score or rejection",
                });
            };
            (
                best,
                SwitchReasonV1::IncumbentIneligible { rejection: reason },
            )
        }
        (Some(best), None) => (best, SwitchReasonV1::BestCandidate),
        (None, _) => {
            if policy.unknown_usage_policy == UnknownUsagePolicyV1::HoldIncumbent {
                if let Some(incumbent) = incumbent_evaluation {
                    if let CandidateDispositionV1::Rejected { reason } = incumbent.disposition {
                        if reason.usage_is_unknown() {
                            return Ok((
                                RouteOutcomeV1::Route {
                                    route: incumbent.candidate.route.clone(),
                                },
                                SwitchReasonV1::IncumbentHeldOnUnknownUsage { rejection: reason },
                            ));
                        }
                    }
                }
            }
            if policy.unknown_usage_policy == UnknownUsagePolicyV1::StaticFallback {
                if let Some(candidate) = canonical_legacy_candidate(input) {
                    let evaluation = evaluations
                        .iter()
                        .find(|evaluation| {
                            evaluation.candidate.route.candidate_id == candidate.route.candidate_id
                        })
                        .ok_or(AdaptiveRoutingError::InvalidInvariant {
                            field: "routing_input.authority.legacy_route",
                            reason: "canonical legacy candidate has no evaluation",
                        })?;
                    if matches!(
                        evaluation.disposition,
                        CandidateDispositionV1::Rejected { reason }
                            if reason.usage_is_unknown()
                    ) {
                        return Ok((
                            RouteOutcomeV1::Route {
                                route: candidate.route.clone(),
                            },
                            SwitchReasonV1::LegacyFallbackOnUnknownUsage,
                        ));
                    }
                }
            }
            return Ok((
                RouteOutcomeV1::Deferred {
                    reason: deferral_reason(evaluations),
                },
                SwitchReasonV1::NoEligibleCandidate,
            ));
        }
    };
    Ok((
        RouteOutcomeV1::Route {
            route: selected.0.candidate.route.clone(),
        },
        selected.1,
    ))
}

/// Computes a deterministic route decision from explicit, versioned policy and input.
pub fn decide_route(
    policy: &AdaptivePolicyV1,
    input: &RoutingInputV1,
) -> Result<RoutingDecisionV1, AdaptiveRoutingError> {
    input.validate_request_invariants()?;

    if let Some(fixed) = &input.authority.resolved_fixed_route {
        let dispatch_outcome = RouteOutcomeV1::Route {
            route: fixed.route.clone(),
        };
        let continuation =
            continuation_for(input.existing_session_runtime.as_ref(), &dispatch_outcome);
        return Ok(RoutingDecisionV1 {
            schema_version: ADAPTIVE_ROUTING_SCHEMA_V1,
            applied_policy: None,
            evaluated_at_utc_ms: input.evaluated_at_utc_ms,
            mission_id: input.mission_id.clone(),
            phase_id: input.phase_id.clone(),
            attempt: input.attempt,
            applied_authority: AppliedAuthorityV1::Fixed {
                provenances: fixed.provenances.clone(),
            },
            requirements: input.requirements.clone(),
            incumbent_candidate_id: input.incumbent_candidate_id.clone(),
            existing_session_runtime: input.existing_session_runtime.clone(),
            legacy_route: None,
            candidate_evaluations: Vec::new(),
            referenced_snapshots: Vec::new(),
            recommended_outcome: dispatch_outcome.clone(),
            dispatch_outcome,
            switch_reason: SwitchReasonV1::FixedAuthority,
            continuation,
            decision_mode: RoutingDecisionModeV1::Fixed,
        });
    }

    policy.validate()?;
    input.validate_adaptive(policy)?;
    let policy = policy.canonicalized();
    let mut candidates = input.candidates.iter().collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.route.candidate_id.cmp(&right.route.candidate_id));
    let mut evaluations = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        evaluations.push(evaluate_candidate(&policy, input, candidate)?);
    }

    let referenced_snapshot_ids = evaluations
        .iter()
        .flat_map(|evaluation| evaluation.considered_snapshot_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut referenced_snapshots = input
        .usage_snapshots
        .iter()
        .filter(|snapshot| referenced_snapshot_ids.contains(&snapshot.snapshot_id))
        .cloned()
        .collect::<Vec<_>>();
    for snapshot in &mut referenced_snapshots {
        snapshot
            .windows
            .sort_by(|left, right| left.limit_id.cmp(&right.limit_id));
    }
    referenced_snapshots.sort_by(|left, right| left.snapshot_id.cmp(&right.snapshot_id));
    let (recommended_outcome, switch_reason) = select_recommendation(&policy, input, &evaluations)?;
    let dispatch_outcome = match policy.evaluation_mode {
        AdaptiveEvaluationModeV1::Enforce => recommended_outcome.clone(),
        AdaptiveEvaluationModeV1::Shadow => RouteOutcomeV1::Route {
            route: input.authority.legacy_route.clone(),
        },
    };
    let continuation = continuation_for(input.existing_session_runtime.as_ref(), &dispatch_outcome);

    Ok(RoutingDecisionV1 {
        schema_version: ADAPTIVE_ROUTING_SCHEMA_V1,
        applied_policy: Some(policy.clone()),
        evaluated_at_utc_ms: input.evaluated_at_utc_ms,
        mission_id: input.mission_id.clone(),
        phase_id: input.phase_id.clone(),
        attempt: input.attempt,
        applied_authority: AppliedAuthorityV1::Adaptive,
        requirements: input.requirements.clone(),
        incumbent_candidate_id: input.incumbent_candidate_id.clone(),
        existing_session_runtime: input.existing_session_runtime.clone(),
        legacy_route: Some(input.authority.legacy_route.clone()),
        candidate_evaluations: evaluations,
        referenced_snapshots,
        recommended_outcome,
        dispatch_outcome,
        switch_reason,
        continuation,
        decision_mode: RoutingDecisionModeV1::Adaptive {
            evaluation_mode: policy.evaluation_mode,
        },
    })
}

/// Recomputes and verifies a version-one replay record.
pub fn replay_route(
    record: &RoutingReplayRecordV1,
) -> Result<RoutingDecisionV1, AdaptiveRoutingError> {
    record.recompute_and_validate()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_rejects_noncanonical_source_provider_reserves()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut record: RoutingReplayRecordV1 = serde_json::from_str(include_str!(
            "../tests/fixtures/adaptive-routing-adaptive-replay-v1.json"
        ))?;
        record.policy.provider_reserves.reverse();
        assert!(matches!(
            replay_route(&record),
            Err(AdaptiveRoutingError::InvalidInvariant {
                field: "routing_replay.policy.provider_reserves",
                ..
            })
        ));
        Ok(())
    }
}
