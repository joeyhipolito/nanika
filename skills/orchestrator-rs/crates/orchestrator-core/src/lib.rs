//! Stable domain vocabulary shared across orchestrator boundaries.
//!
//! Persisted mission state remains data-driven. Only identifiers and wire-format versions
//! are represented as strong types here; dynamic mission transitions are intentionally not
//! encoded as broad typestate.

mod adaptive_routing;
mod barok;
mod codec;
mod configuration;
mod discipline;
mod mission;
mod mission_proposal;
mod persona;
mod ponytail;
mod reducer;
mod router;
mod sanitize;
mod verification;
mod watchdog;

pub use barok::{
    BAROK_ENV_DISABLE, BAROK_PERSONAS, barok_disabled, barok_intensity_tier, barok_rule_card_bytes,
    inject_barok, is_barok_eligible_persona,
};

pub use discipline::{
    DISCIPLINE_ENV_DISABLE, discipline_disabled, discipline_rule_card_bytes, inject_discipline,
};
pub use persona::{PERSONAS_SUBDIR, default_personas_dir, list_persona_names, load_persona_prompt};
pub use ponytail::{
    PONYTAIL_ENV_ENABLE, inject_ponytail, ponytail_enabled, ponytail_rule_card_bytes,
};

pub use codec::{
    CheckpointError, CheckpointPhase, CheckpointPlan, CheckpointProjection, CheckpointSourceShape,
    DecodedCheckpoint, DecodedEvent, EventError, EventJsonMap, EventRecord, EventScan,
    EventScanDiagnostic, EventScanDiagnosticKind, GO_EVENT_JSON_CONTENT_MAX_BYTES, GO_ZERO_TIME,
    GoJsonObjectMember, GoJsonScalar, STABLE_EVENT_TYPES, decode_checkpoint, decode_event_line,
    decode_go_json_array_elements, decode_go_json_object_members, decode_go_json_scalar,
    decode_go_observed_event_line, decode_go_observed_event_record, encode_current_checkpoint,
    encode_current_event, encode_current_plan, encode_preserved_checkpoint, encode_preserved_event,
    go_json_field_matches, normalize_go_time_unmarshal_rfc3339, scan_event_log,
};

pub use adaptive_routing::{
    ADAPTIVE_MAX_DURATION_MS_V1, ADAPTIVE_POLICY_VERSION_V1, ADAPTIVE_ROUTING_SCHEMA_V1,
    AdaptiveCandidateV1, AdaptiveEvaluationModeV1, AdaptivePolicyV1, AdaptiveRoutingError,
    AppliedAuthorityV1, AutomaticEligibilityV1, BasisPointsV1, CandidateDispositionV1,
    CandidateEvaluationV1, CandidateIdV1, CandidateRejectionV1, CapabilityIdV1,
    ContinuationDispositionV1, DurationMillisV1, FixedAuthorityProvenanceV1, FixedRouteV1,
    ProviderIdV1, ProviderReserveV1, QualityTierV1, RouteDeferralReasonV1, RouteOutcomeV1,
    RouteRequirementsV1, RouteTargetV1, RoutingAuthorityV1, RoutingDecisionModeV1,
    RoutingDecisionV1, RoutingInputV1, RoutingReplayRecordV1, RuntimeIdV1, ScoreComponentsV1,
    ScoreWeightsV1, SnapshotIdV1, SwitchReasonV1, TaskPriorityV1, UnknownUsagePolicyV1,
    UsageAccountIdV1, UsageHealthV1, UsageLimitIdV1, UsageReasonCodeV1, UsageSnapshotV1,
    UsageSourceIdV1, UsageWindowKindV1, UsageWindowV1, UtcMillisV1, decide_route, replay_route,
};

pub use configuration::{
    AdvisorConfig, ConfigError, ModelResolutionInput, RoutingMap, RoutingTier, RuntimeResolution,
    RuntimeResolutionInput, RuntimeSource, StallResolutionInput, StallSource,
    StallTimeoutResolution, StallTimeoutValue, resolve_advisor_config, resolve_model,
    resolve_runtime, resolve_stall_timeout,
};
pub use mission::{
    AuthoredParseContext, AuthoredPhase, AuthoredPlanProjection, ExecutionMode, MissionParseError,
    PersonaCatalog, PhasePolicyFixture, RuntimePolicyFixture, TargetContextFixture,
    keyword_fallback_proposal, parse_authored_phases, validate_phase_ids,
};
pub use mission_proposal::{
    CompiledPlan, MAX_PROPOSAL_BYTES, MissionProposal, ProposalError, ProposedPhase,
    compile_proposal,
};
pub use reducer::{
    AppliedEvent, MissionState, MissionStateBuildError, MissionStatus, PhaseDefinition, PhaseState,
    PhaseStatus, ReducerInput, ReducerTransition, Reduction, TransitionError, reduce,
};
pub use router::{
    ModelTier, classify_complexity, classify_tier, claude_model, codex_model, effective_runtime,
    escalate_tier, resolve_effort, resolve_effort_for_runtime, resolve_model_for_runtime,
    select_runtime,
};
pub use sanitize::{Finding, detect_homoglyphs, detect_invisible, has_invisible, sanitize_text};
pub use verification::{
    VerificationAction, VerificationClass, VerificationDecision, VerificationMode,
    VerificationOutcome, VerificationSummary, VerificationTermination, classify_verification,
    decide_verification,
};
pub use watchdog::{
    ActivityKind, ActivitySnapshot, ActivitySource, MonoInstant, MonotonicClock, StdMonotonicClock,
    SupervisorTurnDecision, SupervisorTurnInput, Watchdog, WatchdogDecision, WatchdogError,
    evaluate_supervisor_turn,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{fmt, str::FromStr};
use thiserror::Error;

const MAX_PERSISTED_EVENT_ID_BYTES: usize = 256;

/// Maximum UTF-8 byte length of a worker directory name.
///
/// Worker identities become one filesystem path component, so this matches the
/// 255-byte `NAME_MAX` enforced by the supported filesystems.
pub const MAX_WORKER_ID_BYTES: usize = 255;

/// Errors produced by core domain validation.
#[derive(Debug, Clone, Eq, Error, PartialEq)]
pub enum CoreError {
    /// A stable or persisted compatibility identifier was invalid.
    #[error("invalid {kind} identifier {value:?}: {reason}")]
    InvalidIdentifier {
        /// Identifier category.
        kind: &'static str,
        /// Rejected input.
        value: String,
        /// Stable rejection reason.
        reason: &'static str,
    },
    /// A compatibility version used the reserved zero value.
    #[error("compatibility version must be greater than zero")]
    ZeroCompatibilityVersion,
    /// A version-specific compatibility type received a different wire version.
    #[error("unsupported {surface} version {found}; expected {expected}")]
    UnsupportedCompatibilityVersion {
        /// Compatibility surface being decoded.
        surface: &'static str,
        /// Version found on the wire.
        found: u32,
        /// Only version accepted by the type.
        expected: u32,
    },
}

fn validate_identifier(kind: &'static str, value: &str) -> Result<(), CoreError> {
    if value.is_empty() {
        return Err(CoreError::InvalidIdentifier {
            kind,
            value: value.to_owned(),
            reason: "identifier is empty",
        });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CoreError::InvalidIdentifier {
            kind,
            value: value.to_owned(),
            reason: "only ASCII letters, digits, '-' and '_' are allowed",
        });
    }
    Ok(())
}

fn validate_persisted_event_identifier(value: &str) -> Result<(), CoreError> {
    let reason = if value.is_empty() {
        Some("identifier is empty")
    } else if value.len() > MAX_PERSISTED_EVENT_ID_BYTES {
        Some("persisted event identifier exceeds 256 bytes")
    } else if value.chars().any(char::is_control) {
        Some("persisted event identifier contains a control character")
    } else {
        None
    };
    if let Some(reason) = reason {
        return Err(CoreError::InvalidIdentifier {
            kind: "event",
            value: value.to_owned(),
            reason,
        });
    }
    Ok(())
}

fn validate_worker_identifier(value: &str) -> Result<(), CoreError> {
    let reason = if value.is_empty() {
        Some("identifier is empty")
    } else if value.len() > MAX_WORKER_ID_BYTES {
        Some("worker identifier exceeds 255 bytes")
    } else if matches!(value, "." | "..") {
        Some("worker identifier is a reserved path segment")
    } else if value.contains(['/', '\\']) {
        Some("worker identifier contains a path separator")
    } else if value.chars().any(char::is_control) {
        Some("worker identifier contains a control character")
    } else {
        None
    };
    if let Some(reason) = reason {
        return Err(CoreError::InvalidIdentifier {
            kind: "worker",
            value: value.to_owned(),
            reason,
        });
    }
    Ok(())
}

fn validate_worker_persona_stem(value: &str) -> Result<(), CoreError> {
    let reason = if value.is_empty() {
        Some("identifier is empty")
    } else if value.contains(['/', '\\']) {
        Some("worker identifier contains a path separator")
    } else if value.chars().any(char::is_control) {
        Some("worker identifier contains a control character")
    } else {
        None
    };
    if let Some(reason) = reason {
        return Err(CoreError::InvalidIdentifier {
            kind: "worker",
            value: value.to_owned(),
            reason,
        });
    }
    Ok(())
}

macro_rules! identifier {
    ($name:ident, $kind:literal) => {
        #[doc = concat!("Validated ", $kind, " identifier.")]
        #[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Validates and constructs an identifier.
            pub fn new(value: impl Into<String>) -> Result<Self, CoreError> {
                let value = value.into();
                validate_identifier($kind, &value)?;
                Ok(Self(value))
            }

            /// Returns the stable identifier text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = CoreError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = CoreError;

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

identifier!(MissionId, "mission");
identifier!(PhaseId, "phase");
identifier!(ContractId, "contract");

/// Validated worker directory identifier.
///
/// Unlike mission, phase, and contract identifiers, worker identities retain
/// the full safe UTF-8 filename stem accepted by the Go persona catalog.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct WorkerId(String);

impl WorkerId {
    /// Validates and constructs a path-segment-safe worker identifier.
    ///
    /// Accepted UTF-8 bytes are preserved exactly; this constructor performs no
    /// case folding or Unicode normalization.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidIdentifier`] for an empty or overlong value,
    /// an exact `.` or `..` segment, a path separator, or a control character.
    pub fn new(value: impl Into<String>) -> Result<Self, CoreError> {
        let value = value.into();
        validate_worker_identifier(&value)?;
        Ok(Self(value))
    }

    /// Constructs the deterministic worker identity used by the Go orchestrator.
    ///
    /// The resulting identifier has the exact `<persona>-<phase-id>` shape and
    /// preserves the validated input bytes without normalization.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidIdentifier`] when `persona` is empty or
    /// contains a path separator or control character, or when the composed
    /// value violates the strict [`WorkerId`] syntax.
    pub fn for_phase(persona: &str, phase: &PhaseId) -> Result<Self, CoreError> {
        validate_worker_persona_stem(persona)?;
        let Some(composed_len) = persona
            .len()
            .checked_add(1)
            .and_then(|length| length.checked_add(phase.as_str().len()))
        else {
            return Err(CoreError::InvalidIdentifier {
                kind: "worker",
                value: persona.to_owned(),
                reason: "worker identifier exceeds 255 bytes",
            });
        };
        if composed_len > MAX_WORKER_ID_BYTES {
            return Err(CoreError::InvalidIdentifier {
                kind: "worker",
                value: persona.to_owned(),
                reason: "worker identifier exceeds 255 bytes",
            });
        }

        let mut worker = String::with_capacity(composed_len);
        worker.push_str(persona);
        worker.push('-');
        worker.push_str(phase.as_str());
        Self::new(worker)
    }

    /// Returns the stable identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for WorkerId {
    type Err = CoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for WorkerId {
    type Error = CoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<WorkerId> for String {
    fn from(value: WorkerId) -> Self {
        value.0
    }
}

/// Validated event identifier.
///
/// [`EventId::new`] and string parsing are strict live-construction boundaries.
/// Deserialization accepts the wider persisted compatibility shape so a legacy
/// event identity can serialize and deserialize without becoming invalid.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(into = "String")]
pub struct EventId(String);

impl EventId {
    /// Validates and constructs a new live event identifier.
    pub fn new(value: impl Into<String>) -> Result<Self, CoreError> {
        let value = value.into();
        validate_identifier("event", &value)?;
        Ok(Self(value))
    }

    fn from_persisted(value: impl Into<String>) -> Result<Self, CoreError> {
        let value = value.into();
        validate_persisted_event_identifier(&value)?;
        Ok(Self(value))
    }

    /// Returns the stable identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for EventId {
    type Err = CoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for EventId {
    type Error = CoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<EventId> for String {
    fn from(value: EventId) -> Self {
        value.0
    }
}

impl<'de> Deserialize<'de> for EventId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_persisted(value).map_err(de::Error::custom)
    }
}

impl DecodedEvent {
    /// Reconstructs the event identity at the persisted compatibility boundary.
    ///
    /// Historical Go envelopes may contain printable IDs that predate the
    /// current `evt_...` writer shape. New live IDs remain restricted to
    /// [`EventId::new`].
    pub fn persisted_event_id(&self) -> Result<EventId, CoreError> {
        EventId::from_persisted(self.record.id.clone())
    }
}

/// A non-zero version carried by a compatibility surface.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct CompatibilityVersion(u32);

impl CompatibilityVersion {
    /// Constructs a supported version number.
    pub const fn new(value: u32) -> Result<Self, CoreError> {
        if value == 0 {
            Err(CoreError::ZeroCompatibilityVersion)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the integer wire value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for CompatibilityVersion {
    type Error = CoreError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<CompatibilityVersion> for u32 {
    fn from(value: CompatibilityVersion) -> Self {
        value.get()
    }
}

/// Wire marker that can only encode or decode compatibility envelope version 1.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EnvelopeVersionV1;

impl Serialize for EnvelopeVersionV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u32(1)
    }
}

impl<'de> Deserialize<'de> for EnvelopeVersionV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let found = u32::deserialize(deserializer)?;
        if found == 1 {
            Ok(Self)
        } else {
            Err(de::Error::custom(
                CoreError::UnsupportedCompatibilityVersion {
                    surface: "compatibility envelope",
                    found,
                    expected: 1,
                },
            ))
        }
    }
}

/// Version-one envelope used by versioned compatibility projections.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompatibilityEnvelopeV1<T> {
    /// Envelope schema version, statically restricted to wire value 1.
    pub version: EnvelopeVersionV1,
    /// Versioned compatibility payload.
    pub payload: T,
}

impl<T> CompatibilityEnvelopeV1<T> {
    /// Current envelope version.
    pub const VERSION: u32 = 1;

    /// Wraps a payload in the current envelope.
    pub const fn new(payload: T) -> Self {
        Self {
            version: EnvelopeVersionV1,
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CompatibilityEnvelopeV1, CompatibilityVersion, CoreError, EnvelopeVersionV1,
        MAX_WORKER_ID_BYTES, MissionId, PhaseId, WorkerId,
    };
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
    struct FixturePayload {
        name: String,
        retained_unknown_field: bool,
    }

    #[test]
    fn identifiers_are_distinct_validated_types() -> Result<(), Box<dyn std::error::Error>> {
        let mission = MissionId::new("20260713-41c522c4")?;
        let phase = PhaseId::new("design_rust_runtime")?;
        let worker = WorkerId::new("worker-01")?;

        assert_eq!(mission.as_str(), "20260713-41c522c4");
        assert_eq!(phase.as_str(), "design_rust_runtime");
        assert_eq!(worker.as_str(), "worker-01");
        assert!(MissionId::new("../escape").is_err());
        Ok(())
    }

    #[test]
    fn worker_id_for_phase_matches_go_worker_name_convention()
    -> Result<(), Box<dyn std::error::Error>> {
        let phase = PhaseId::new("phase-1")?;

        let worker = WorkerId::for_phase("architect", &phase)?;

        assert_eq!(worker.as_str(), "architect-phase-1");
        Ok(())
    }

    #[test]
    fn worker_id_for_phase_accepts_cpp_persona_stem() -> Result<(), Box<dyn std::error::Error>> {
        let phase = PhaseId::new("phase-1")?;

        let worker = WorkerId::for_phase("C++", &phase)?;

        assert_eq!(worker.as_str(), "C++-phase-1");
        Ok(())
    }

    #[test]
    fn worker_id_for_phase_accepts_persona_stem_with_spaces()
    -> Result<(), Box<dyn std::error::Error>> {
        let phase = PhaseId::new("phase-1")?;

        let worker = WorkerId::for_phase("data science", &phase)?;

        assert_eq!(worker.as_str(), "data science-phase-1");
        Ok(())
    }

    #[test]
    fn worker_id_for_phase_preserves_unicode_bytes_without_normalization()
    -> Result<(), Box<dyn std::error::Error>> {
        let phase = PhaseId::new("phase-1")?;
        let persona = "研究者-e\u{301}";

        let worker = WorkerId::for_phase(persona, &phase)?;

        assert_eq!(
            worker.as_str().as_bytes(),
            "研究者-e\u{301}-phase-1".as_bytes()
        );
        Ok(())
    }

    #[test]
    fn worker_id_serde_round_trip_preserves_safe_filename_stem()
    -> Result<(), Box<dyn std::error::Error>> {
        let worker = WorkerId::new("data science-phase-1")?;

        let encoded = serde_json::to_string(&worker)?;
        let decoded = serde_json::from_str::<WorkerId>(&encoded)?;

        assert_eq!(decoded, worker);
        Ok(())
    }

    #[test]
    fn worker_id_for_phase_rejects_empty_persona_before_composition()
    -> Result<(), Box<dyn std::error::Error>> {
        let phase = PhaseId::new("phase-01")?;

        let Err(error) = WorkerId::for_phase("", &phase) else {
            return Err("an empty persona produced a worker identity".into());
        };

        assert_eq!(
            error,
            CoreError::InvalidIdentifier {
                kind: "worker",
                value: String::new(),
                reason: "identifier is empty",
            }
        );
        Ok(())
    }

    #[test]
    fn worker_id_for_phase_rejects_persona_path_separators()
    -> Result<(), Box<dyn std::error::Error>> {
        let phase = PhaseId::new("phase-01")?;

        for persona in ["team/lead", r"team\lead"] {
            let Err(error) = WorkerId::for_phase(persona, &phase) else {
                return Err("a persona with path separators produced a worker identity".into());
            };
            assert_eq!(
                error,
                CoreError::InvalidIdentifier {
                    kind: "worker",
                    value: persona.to_owned(),
                    reason: "worker identifier contains a path separator",
                }
            );
        }
        Ok(())
    }

    #[test]
    fn worker_id_for_phase_accepts_single_dot_persona_stem()
    -> Result<(), Box<dyn std::error::Error>> {
        let phase = PhaseId::new("phase-1")?;

        let worker = WorkerId::for_phase(".", &phase)?;

        assert_eq!(worker.as_str().as_bytes(), b".-phase-1");
        Ok(())
    }

    #[test]
    fn worker_id_for_phase_accepts_double_dot_persona_stem()
    -> Result<(), Box<dyn std::error::Error>> {
        let phase = PhaseId::new("phase-1")?;

        let worker = WorkerId::for_phase("..", &phase)?;

        assert_eq!(worker.as_str().as_bytes(), b"..-phase-1");
        Ok(())
    }

    #[test]
    fn worker_id_for_phase_rejects_persona_control_characters()
    -> Result<(), Box<dyn std::error::Error>> {
        let phase = PhaseId::new("phase-01")?;

        for persona in ["reviewer\nforged", "reviewer\0forged"] {
            let Err(error) = WorkerId::for_phase(persona, &phase) else {
                return Err("a persona with control characters produced a worker identity".into());
            };
            assert_eq!(
                error,
                CoreError::InvalidIdentifier {
                    kind: "worker",
                    value: persona.to_owned(),
                    reason: "worker identifier contains a control character",
                }
            );
        }
        Ok(())
    }

    #[test]
    fn worker_id_new_accepts_exact_255_byte_limit() -> Result<(), Box<dyn std::error::Error>> {
        let worker = WorkerId::new("w".repeat(MAX_WORKER_ID_BYTES))?;

        assert_eq!(worker.as_str().len(), MAX_WORKER_ID_BYTES);
        Ok(())
    }

    #[test]
    fn worker_id_new_rejects_256_bytes() -> Result<(), Box<dyn std::error::Error>> {
        let value = "w".repeat(MAX_WORKER_ID_BYTES + 1);

        let Err(error) = WorkerId::new(value.clone()) else {
            return Err("a 256-byte worker identifier was accepted".into());
        };

        assert_eq!(
            error,
            CoreError::InvalidIdentifier {
                kind: "worker",
                value,
                reason: "worker identifier exceeds 255 bytes",
            }
        );
        Ok(())
    }

    #[test]
    fn worker_id_for_phase_accepts_exact_255_byte_composition()
    -> Result<(), Box<dyn std::error::Error>> {
        let phase = PhaseId::new("phase-1")?;
        let persona = "p".repeat(MAX_WORKER_ID_BYTES - 1 - phase.as_str().len());

        let worker = WorkerId::for_phase(&persona, &phase)?;

        assert_eq!(worker.as_str().len(), MAX_WORKER_ID_BYTES);
        Ok(())
    }

    #[test]
    fn worker_id_for_phase_rejects_256_byte_composition() -> Result<(), Box<dyn std::error::Error>>
    {
        let phase = PhaseId::new("phase-1")?;
        let persona = "p".repeat(MAX_WORKER_ID_BYTES - phase.as_str().len());

        let Err(error) = WorkerId::for_phase(&persona, &phase) else {
            return Err("a 256-byte composed worker identifier was accepted".into());
        };

        assert_eq!(
            error,
            CoreError::InvalidIdentifier {
                kind: "worker",
                value: persona,
                reason: "worker identifier exceeds 255 bytes",
            }
        );
        Ok(())
    }

    #[test]
    fn compatibility_versions_are_non_zero_and_serialize_as_integers()
    -> Result<(), Box<dyn std::error::Error>> {
        assert!(CompatibilityVersion::new(0).is_err());
        let version = CompatibilityVersion::new(7)?;
        assert_eq!(serde_json::to_string(&version)?, "7");
        assert_eq!(serde_json::from_str::<CompatibilityVersion>("7")?, version);
        assert!(serde_json::from_str::<CompatibilityVersion>("0").is_err());
        Ok(())
    }

    #[test]
    fn version_one_envelope_round_trips_and_rejects_every_other_version()
    -> Result<(), Box<dyn std::error::Error>> {
        let envelope = CompatibilityEnvelopeV1::new(FixturePayload {
            name: "checkpoint".to_owned(),
            retained_unknown_field: true,
        });
        let encoded = serde_json::to_string(&envelope)?;
        assert_eq!(
            encoded,
            r#"{"version":1,"payload":{"name":"checkpoint","retained_unknown_field":true}}"#
        );
        assert_eq!(
            serde_json::from_str::<CompatibilityEnvelopeV1<FixturePayload>>(&encoded)?,
            envelope
        );

        for unsupported in [0, 2, u32::MAX] {
            let encoded = format!(
                r#"{{"version":{unsupported},"payload":{{"name":"checkpoint","retained_unknown_field":true}}}}"#
            );
            let error =
                match serde_json::from_str::<CompatibilityEnvelopeV1<FixturePayload>>(&encoded) {
                    Ok(_) => {
                        return Err(format!(
                            "unsupported envelope version {unsupported} decoded successfully"
                        )
                        .into());
                    }
                    Err(error) => error,
                };
            assert!(
                error.to_string().contains(&format!(
                    "unsupported compatibility envelope version {unsupported}; expected 1"
                )),
                "unexpected error for version {unsupported}: {error}"
            );
        }

        assert_eq!(EnvelopeVersionV1, EnvelopeVersionV1);
        Ok(())
    }
}
