//! Validated phase-executor contract.
//!
//! Provider executors receive only a registry-bound [`DispatchRequest`]. This
//! keeps requested-runtime fallback, effective runtime, and provider-bound
//! continuation in one enforced path. Attempt completion and partial work are
//! different types, so a "partial completed" outcome cannot be constructed.

use std::fmt;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(unix)]
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::authority::{
    Clock, EffectBudget, EffectReceipt, EffectRequest, EffectService, EffectServiceError,
    EffectServiceErrorKind, EvidenceVerificationRequest, EvidenceVerifier, EvidenceVerifierError,
    EvidenceVerifierErrorKind, ProcessBudget, ProcessExitStatus, ProcessPreflight,
    ProcessPreflightReason, ProcessPurpose, ProcessReceipt, ProcessRequest, ProcessService,
    ProcessServiceError, ProcessServiceErrorKind, ProcessTerminationReceipt, WatchdogDecision,
    WatchdogPolicy,
};
use crate::event::{
    EventReceipt, EventSink, EventSinkError, WorkerCompleted, WorkerEventDraft, WorkerEventError,
    WorkerEventKind, WorkerEventPayload, WorkerFailed, WorkerFailedFields, WorkerIdentity,
    WorkerSpawned, WorkerSpawnedFields,
};

const MAX_RUNTIME_FAMILY_BYTES: usize = 64;
const MAX_SESSION_ID_BYTES: usize = 4 * 1024;
const MAX_ID_BYTES: usize = 256;
const MAX_LABEL_BYTES: usize = 4 * 1024;
const MAX_MODEL_BYTES: usize = 256;
const MAX_REQUEST_TEXT_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_LIST_ITEMS: usize = 512;
const MAX_FINAL_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOOL_SUMMARY_BYTES: usize = 16 * 1024;
const MAX_TOOL_OBSERVATIONS: usize = 4_096;
const MAX_ARTIFACT_RECEIPTS: usize = 4_096;
const MAX_EFFECT_RECEIPTS: usize = 4_096;
const MAX_DIGEST_BYTES: usize = 512;
const MAX_FAILURE_DETAIL_BYTES: usize = 16 * 1024;
const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_TURNS: u64 = 1_000_000;
const MAX_AUTHORITATIVE_FAILURES: usize = 32;
const EXECUTION_REQUEST_FINGERPRINT_SCHEMA_VERSION: u8 = 1;
#[cfg(unix)]
const EXECUTION_REQUEST_FINGERPRINT_DOMAIN: &[u8] = b"nanika-hermetic-execution-binding/v1";

/// Reasoning effort accepted by the current Go runtimes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
}

impl Effort {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }
}

/// Validated runtime/provider family used for registry and session binding.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RuntimeFamily(String);

impl RuntimeFamily {
    /// Parses a lowercase runtime key. Runtime keys are one to 64 bytes and
    /// contain only ASCII alphanumerics plus `.`, `_`, and `-`.
    pub fn parse(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ContractError::Empty {
                field: "runtime_family",
            });
        }
        if value.len() > MAX_RUNTIME_FAMILY_BYTES {
            return Err(ContractError::TooLong {
                field: "runtime_family",
                max: MAX_RUNTIME_FAMILY_BYTES,
            });
        }
        let mut bytes = value.bytes();
        let Some(first) = bytes.next() else {
            return Err(ContractError::Empty {
                field: "runtime_family",
            });
        };
        if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
            return Err(ContractError::InvalidRuntimeFamily);
        }
        if bytes.any(|byte| {
            !byte.is_ascii_lowercase()
                && !byte.is_ascii_digit()
                && !matches!(byte, b'.' | b'_' | b'-')
        }) {
            return Err(ContractError::InvalidRuntimeFamily);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RuntimeFamily {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("RuntimeFamily")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for RuntimeFamily {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<&str> for RuntimeFamily {
    type Error = ContractError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

/// A provider-bound continuation handle. Its ID is never included in ordinary
/// `Debug` or `Display` output.
#[derive(Clone, Eq, PartialEq)]
pub struct SessionHandle {
    runtime_family: RuntimeFamily,
    session_id: String,
}

impl SessionHandle {
    pub fn new(
        runtime_family: RuntimeFamily,
        session_id: impl Into<String>,
    ) -> Result<Self, ContractError> {
        let session_id = session_id.into();
        validate_required_content("session_id", &session_id, MAX_SESSION_ID_BYTES)?;
        Ok(Self {
            runtime_family,
            session_id,
        })
    }

    #[must_use]
    pub fn runtime_family(&self) -> &RuntimeFamily {
        &self.runtime_family
    }

    /// Explicitly exposes the provider ID for the matching provider adapter.
    #[must_use]
    pub fn expose_session_id(&self) -> &str {
        &self.session_id
    }

    #[must_use]
    pub fn can_resume_into(&self, runtime: &RuntimeFamily) -> bool {
        &self.runtime_family == runtime
    }
}

impl fmt::Debug for SessionHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionHandle")
            .field("runtime_family", &self.runtime_family)
            .field("session_id", &"[REDACTED]")
            .field("session_id_len", &self.session_id.len())
            .finish()
    }
}

/// Unvalidated input consumed by [`ExecutionRequest::new`]. The resulting
/// request has private fields and cannot represent zero attempts/revisions or a
/// cross-provider resume handle.
pub struct ExecutionRequestDraft {
    pub mission: String,
    pub phase: String,
    pub attempt: u32,
    pub revision: u32,
    pub objective: String,
    pub persona: String,
    pub role: String,
    pub domain: String,
    pub skills: Vec<String>,
    pub dependencies: Vec<String>,
    pub expected_evidence: Vec<String>,
    pub constraints: Vec<String>,
    pub prior_context: String,
    /// Effective runtime returned by the registry, not the unknown requested
    /// key that may have triggered a fallback.
    pub runtime: RuntimeFamily,
    pub model: String,
    pub effort: Effort,
    /// Zero means "use the engine default", matching Go.
    pub max_turns: u64,
    pub worker_dir: PathBuf,
    pub target_dir: Option<PathBuf>,
    pub resume_from: Option<SessionHandle>,
    pub hook_script: Option<PathBuf>,
}

/// Stable digest binding one worker to every semantic input of an
/// [`ExecutionRequest`].
///
/// The digest is deliberately opaque. Its `Debug` implementation never
/// reveals request material, while [`Self::to_lowercase_hex`] is reserved for
/// persistence boundaries that need the existing lowercase SHA-256 encoding.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct ExecutionRequestFingerprint([u8; 32]);

impl ExecutionRequestFingerprint {
    /// Returns the version of the persisted fingerprint schema.
    #[must_use]
    pub const fn schema_version() -> u8 {
        EXECUTION_REQUEST_FINGERPRINT_SCHEMA_VERSION
    }

    /// Encodes the digest for the existing persistence representation.
    ///
    /// Ordinary diagnostics should use the redacted `Debug` implementation.
    #[must_use]
    pub fn to_lowercase_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";

        let mut encoded = String::with_capacity(self.0.len().saturating_mul(2));
        for byte in self.0 {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        encoded
    }
}

impl fmt::Debug for ExecutionRequestFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ExecutionRequestFingerprint([REDACTED])")
    }
}

/// Validated, redacted inputs for one phase attempt.
pub struct ExecutionRequest {
    mission: String,
    phase: String,
    attempt: u32,
    revision: u32,
    objective: String,
    persona: String,
    role: String,
    domain: String,
    skills: Vec<String>,
    dependencies: Vec<String>,
    expected_evidence: Vec<String>,
    constraints: Vec<String>,
    prior_context: String,
    runtime: RuntimeFamily,
    model: String,
    effort: Effort,
    max_turns: u64,
    worker_dir: PathBuf,
    target_dir: Option<PathBuf>,
    resume_from: Option<SessionHandle>,
    hook_script: Option<PathBuf>,
    worker_spawned: WorkerSpawned,
}

impl ExecutionRequest {
    pub fn new(draft: ExecutionRequestDraft) -> Result<Self, ContractError> {
        validate_required_label("mission", &draft.mission, MAX_ID_BYTES)?;
        validate_required_label("phase", &draft.phase, MAX_ID_BYTES)?;
        if draft.attempt == 0 {
            return Err(ContractError::ZeroOrdinal { field: "attempt" });
        }
        if draft.revision == 0 {
            return Err(ContractError::ZeroOrdinal { field: "revision" });
        }
        validate_required_content("objective", &draft.objective, MAX_REQUEST_TEXT_BYTES)?;
        validate_required_label("persona", &draft.persona, MAX_LABEL_BYTES)?;
        validate_required_label("role", &draft.role, MAX_LABEL_BYTES)?;
        validate_required_label("domain", &draft.domain, MAX_LABEL_BYTES)?;
        validate_list("skills", &draft.skills)?;
        validate_list("dependencies", &draft.dependencies)?;
        validate_list("expected_evidence", &draft.expected_evidence)?;
        validate_list("constraints", &draft.constraints)?;
        validate_content(
            "prior_context",
            &draft.prior_context,
            MAX_REQUEST_TEXT_BYTES,
        )?;
        validate_content("model", &draft.model, MAX_MODEL_BYTES)?;
        if draft.max_turns > MAX_TURNS {
            return Err(ContractError::ValueTooLarge {
                field: "max_turns",
                max: MAX_TURNS,
            });
        }
        validate_path("worker_dir", &draft.worker_dir)?;
        if let Some(path) = draft.target_dir.as_deref() {
            validate_path("target_dir", path)?;
        }
        if let Some(path) = draft.hook_script.as_deref() {
            validate_path("hook_script", path)?;
        }
        if let Some(session) = draft.resume_from.as_ref() {
            if !session.can_resume_into(&draft.runtime) {
                return Err(ContractError::SessionRuntimeMismatch);
            }
        }
        let worker_spawned = WorkerSpawned::new(WorkerSpawnedFields {
            model: draft.model.clone(),
            runtime: Some(draft.runtime.clone()),
            effort_level: Some(draft.effort),
            persona: Some(draft.persona.clone()),
            directory: draft.worker_dir.to_string_lossy().into_owned(),
        })
        .map_err(worker_spawned_contract_error)?;

        Ok(Self {
            mission: draft.mission,
            phase: draft.phase,
            attempt: draft.attempt,
            revision: draft.revision,
            objective: draft.objective,
            persona: draft.persona,
            role: draft.role,
            domain: draft.domain,
            skills: draft.skills,
            dependencies: draft.dependencies,
            expected_evidence: draft.expected_evidence,
            constraints: draft.constraints,
            prior_context: draft.prior_context,
            runtime: draft.runtime,
            model: draft.model,
            effort: draft.effort,
            max_turns: draft.max_turns,
            worker_dir: draft.worker_dir,
            target_dir: draft.target_dir,
            resume_from: draft.resume_from,
            hook_script: draft.hook_script,
            worker_spawned,
        })
    }

    #[must_use]
    pub fn mission(&self) -> &str {
        &self.mission
    }

    #[must_use]
    pub fn phase(&self) -> &str {
        &self.phase
    }

    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    #[must_use]
    pub const fn revision(&self) -> u32 {
        self.revision
    }

    #[must_use]
    pub fn objective(&self) -> &str {
        &self.objective
    }

    #[must_use]
    pub fn persona(&self) -> &str {
        &self.persona
    }

    #[must_use]
    pub fn role(&self) -> &str {
        &self.role
    }

    #[must_use]
    pub fn domain(&self) -> &str {
        &self.domain
    }

    #[must_use]
    pub fn skills(&self) -> &[String] {
        &self.skills
    }

    #[must_use]
    pub fn dependencies(&self) -> &[String] {
        &self.dependencies
    }

    #[must_use]
    pub fn expected_evidence(&self) -> &[String] {
        &self.expected_evidence
    }

    #[must_use]
    pub fn constraints(&self) -> &[String] {
        &self.constraints
    }

    #[must_use]
    pub fn prior_context(&self) -> &str {
        &self.prior_context
    }

    #[must_use]
    pub fn runtime(&self) -> &RuntimeFamily {
        &self.runtime
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    #[must_use]
    pub const fn effort(&self) -> Effort {
        self.effort
    }

    #[must_use]
    pub const fn max_turns(&self) -> u64 {
        self.max_turns
    }

    #[must_use]
    pub fn worker_dir(&self) -> &Path {
        &self.worker_dir
    }

    #[must_use]
    pub fn target_dir(&self) -> Option<&Path> {
        self.target_dir.as_deref()
    }

    #[must_use]
    pub fn resume_from(&self) -> Option<&SessionHandle> {
        self.resume_from.as_ref()
    }

    #[must_use]
    pub fn hook_script(&self) -> Option<&Path> {
        self.hook_script.as_deref()
    }

    /// Computes the versioned execution binding used by durable persistence.
    ///
    /// `requested_runtime` is the raw registry key, before fallback resolution;
    /// the request's `runtime` field is the effective runtime. Every request
    /// field is bound in fixed order with length framing. Unix paths retain
    /// their exact native bytes.
    #[cfg(unix)]
    #[must_use]
    pub fn fingerprint(
        &self,
        worker_id: &str,
        requested_runtime: &str,
    ) -> ExecutionRequestFingerprint {
        let Self {
            mission,
            phase,
            attempt,
            revision,
            objective,
            persona,
            role,
            domain,
            skills,
            dependencies,
            expected_evidence,
            constraints,
            prior_context,
            runtime,
            model,
            effort,
            max_turns,
            worker_dir,
            target_dir,
            resume_from,
            hook_script,
            worker_spawned: _,
        } = self;

        let mut digest = Sha256::new();
        hash_execution_frame(&mut digest, EXECUTION_REQUEST_FINGERPRINT_DOMAIN);
        hash_execution_named(&mut digest, b"worker_id", worker_id.as_bytes());
        hash_execution_named(
            &mut digest,
            b"requested_runtime",
            requested_runtime.as_bytes(),
        );
        hash_execution_named(&mut digest, b"mission", mission.as_bytes());
        hash_execution_named(&mut digest, b"phase", phase.as_bytes());
        hash_execution_named(&mut digest, b"attempt", &attempt.to_be_bytes());
        hash_execution_named(&mut digest, b"revision", &revision.to_be_bytes());
        hash_execution_named(&mut digest, b"objective", objective.as_bytes());
        hash_execution_named(&mut digest, b"persona", persona.as_bytes());
        hash_execution_named(&mut digest, b"role", role.as_bytes());
        hash_execution_named(&mut digest, b"domain", domain.as_bytes());
        hash_execution_string_list(&mut digest, b"skills", skills);
        hash_execution_string_list(&mut digest, b"dependencies", dependencies);
        hash_execution_string_list(&mut digest, b"expected_evidence", expected_evidence);
        hash_execution_string_list(&mut digest, b"constraints", constraints);
        hash_execution_named(&mut digest, b"prior_context", prior_context.as_bytes());
        hash_execution_named(&mut digest, b"runtime", runtime.as_str().as_bytes());
        hash_execution_named(&mut digest, b"model", model.as_bytes());
        hash_execution_named(&mut digest, b"effort", effort.as_str().as_bytes());
        hash_execution_named(&mut digest, b"max_turns", &max_turns.to_be_bytes());
        hash_execution_named(
            &mut digest,
            b"worker_dir",
            worker_dir.as_os_str().as_bytes(),
        );
        hash_execution_optional_path(&mut digest, b"target_dir", target_dir.as_deref());
        match resume_from {
            None => hash_execution_named(&mut digest, b"resume_from", &[0]),
            Some(session) => {
                hash_execution_named(&mut digest, b"resume_from", &[1]);
                hash_execution_named(
                    &mut digest,
                    b"resume_runtime",
                    session.runtime_family().as_str().as_bytes(),
                );
                hash_execution_named(
                    &mut digest,
                    b"resume_session_id",
                    session.expose_session_id().as_bytes(),
                );
            }
        }
        hash_execution_optional_path(&mut digest, b"hook_script", hook_script.as_deref());
        ExecutionRequestFingerprint(digest.finalize().into())
    }

    fn worker_spawned(&self) -> WorkerSpawned {
        self.worker_spawned.clone()
    }
}

#[cfg(unix)]
fn hash_execution_frame(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
}

#[cfg(unix)]
fn hash_execution_named(digest: &mut Sha256, name: &[u8], value: &[u8]) {
    hash_execution_frame(digest, name);
    hash_execution_frame(digest, value);
}

#[cfg(unix)]
fn hash_execution_string_list(digest: &mut Sha256, name: &[u8], values: &[String]) {
    hash_execution_frame(digest, name);
    hash_execution_frame(
        digest,
        &u64::try_from(values.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for value in values {
        hash_execution_frame(digest, value.as_bytes());
    }
}

#[cfg(unix)]
fn hash_execution_optional_path(digest: &mut Sha256, name: &[u8], path: Option<&Path>) {
    hash_execution_frame(digest, name);
    match path {
        None => hash_execution_frame(digest, &[0]),
        Some(path) => {
            hash_execution_frame(digest, &[1]);
            hash_execution_frame(digest, path.as_os_str().as_bytes());
        }
    }
}

impl fmt::Debug for ExecutionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionRequest")
            .field("mission_len", &self.mission.len())
            .field("phase_len", &self.phase.len())
            .field("attempt", &self.attempt)
            .field("revision", &self.revision)
            .field("runtime", &self.runtime)
            .field("model_len", &self.model.len())
            .field("effort", &self.effort)
            .field("objective_len", &self.objective.len())
            .field("prior_context_len", &self.prior_context.len())
            .field("worker_dir", &"[REDACTED]")
            .field(
                "target_dir",
                &self.target_dir.as_ref().map(|_| "[REDACTED]"),
            )
            .field("has_resume_session", &self.resume_from.is_some())
            .finish_non_exhaustive()
    }
}

/// Token/cost telemetry captured from the worker.
#[derive(Clone, Default, PartialEq)]
pub struct CostInfo {
    input_tokens: u64,
    output_tokens: u64,
    total_cost_usd: f64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
}

impl CostInfo {
    pub fn new(
        input_tokens: u64,
        output_tokens: u64,
        total_cost_usd: f64,
        cache_creation_tokens: u64,
        cache_read_tokens: u64,
    ) -> Result<Self, ContractError> {
        if !total_cost_usd.is_finite() || total_cost_usd < 0.0 {
            return Err(ContractError::InvalidCost);
        }
        Ok(Self {
            input_tokens,
            output_tokens,
            total_cost_usd,
            cache_creation_tokens,
            cache_read_tokens,
        })
    }

    #[must_use]
    pub const fn input_tokens(&self) -> u64 {
        self.input_tokens
    }

    #[must_use]
    pub const fn output_tokens(&self) -> u64 {
        self.output_tokens
    }

    #[must_use]
    pub const fn total_cost_usd(&self) -> f64 {
        self.total_cost_usd
    }

    #[must_use]
    pub const fn cache_creation_tokens(&self) -> u64 {
        self.cache_creation_tokens
    }

    #[must_use]
    pub const fn cache_read_tokens(&self) -> u64 {
        self.cache_read_tokens
    }
}

impl fmt::Debug for CostInfo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CostInfo")
            .field("input_tokens", &self.input_tokens)
            .field("output_tokens", &self.output_tokens)
            .field("total_cost_usd", &self.total_cost_usd)
            .field("cache_creation_tokens", &self.cache_creation_tokens)
            .field("cache_read_tokens", &self.cache_read_tokens)
            .finish()
    }
}

/// One bounded tool invocation observation.
#[derive(Clone, Eq, PartialEq)]
pub struct ToolObservation {
    tool_name: String,
    exit_status: Option<i32>,
    summary: String,
}

impl ToolObservation {
    pub fn new(
        tool_name: impl Into<String>,
        exit_status: Option<i32>,
        summary: impl Into<String>,
    ) -> Result<Self, ContractError> {
        let tool_name = tool_name.into();
        let summary = summary.into();
        validate_required_label("tool_name", &tool_name, MAX_LABEL_BYTES)?;
        validate_content("tool_summary", &summary, MAX_TOOL_SUMMARY_BYTES)?;
        Ok(Self {
            tool_name,
            exit_status,
            summary,
        })
    }

    #[must_use]
    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    #[must_use]
    pub const fn exit_status(&self) -> Option<i32> {
        self.exit_status
    }

    #[must_use]
    pub fn expose_summary(&self) -> &str {
        &self.summary
    }
}

impl fmt::Debug for ToolObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolObservation")
            .field("tool_name_len", &self.tool_name.len())
            .field("exit_status", &self.exit_status)
            .field("summary", &"[REDACTED]")
            .field("summary_len", &self.summary.len())
            .finish()
    }
}

/// Provider artifact claim or authority-qualified artifact receipt.
///
/// [`ArtifactReceipt::new`] creates an unverified claim. Only an
/// [`EvidenceVerifier`] selected by the composition root can qualify it during
/// registry dispatch.
#[derive(Clone, Eq, PartialEq)]
pub struct ArtifactReceipt {
    path: PathBuf,
    digest: String,
    bytes: u64,
    authority_verified: bool,
}

impl ArtifactReceipt {
    pub fn new(
        path: impl Into<PathBuf>,
        digest: impl Into<String>,
        bytes: u64,
    ) -> Result<Self, ContractError> {
        let path = path.into();
        validate_path("artifact_path", &path)?;
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(ContractError::UnnormalizedArtifactPath);
        }
        let digest = digest.into();
        validate_required_label("artifact_digest", &digest, MAX_DIGEST_BYTES)?;
        Ok(Self {
            path,
            digest,
            bytes,
            authority_verified: false,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    #[must_use]
    pub const fn is_authority_verified(&self) -> bool {
        self.authority_verified
    }

    fn mark_authority_verified(&mut self) {
        self.authority_verified = true;
    }

    fn mark_unverified(&mut self) {
        self.authority_verified = false;
    }
}

impl fmt::Debug for ArtifactReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactReceipt")
            .field("path", &"[REDACTED]")
            .field("digest", &"[REDACTED]")
            .field("bytes", &self.bytes)
            .field("authority_verified", &self.authority_verified)
            .finish()
    }
}

/// Closed policy/failure classification. Mechanical cancellation, deadlines,
/// stalls, and process exit remain in [`MechanicalTermination`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureKind {
    Capacity,
    Authentication,
    Permission,
    Transport,
    MissingTool,
    DependencyMissing,
    UpstreamInvalidated,
    ArtifactConflict,
    GitConflict,
    HumanDecisionRequired,
    Parse,
    Protocol,
    Semantic,
    Verification,
    Infrastructure,
}

impl fmt::Display for FailureKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Capacity => "provider capacity",
            Self::Authentication => "authentication",
            Self::Permission => "permission",
            Self::Transport => "transport",
            Self::MissingTool => "missing tool or capability",
            Self::DependencyMissing => "dependency missing",
            Self::UpstreamInvalidated => "upstream invalidated",
            Self::ArtifactConflict => "artifact conflict",
            Self::GitConflict => "git conflict",
            Self::HumanDecisionRequired => "human decision required",
            Self::Parse => "parse",
            Self::Protocol => "protocol",
            Self::Semantic => "semantic",
            Self::Verification => "verification",
            Self::Infrastructure => "infrastructure",
        };
        formatter.write_str(label)
    }
}

/// Typed failure with bounded detail that is redacted from normal formatting.
#[derive(Clone, Error)]
#[error("attempt failed: {kind}")]
pub struct Failure {
    kind: FailureKind,
    detail: String,
}

impl Failure {
    pub fn new(kind: FailureKind, detail: impl Into<String>) -> Result<Self, ContractError> {
        let detail = detail.into();
        validate_required_content("failure_detail", &detail, MAX_FAILURE_DETAIL_BYTES)?;
        Ok(Self { kind, detail })
    }

    pub(crate) fn event_delivery(error: &EventSinkError) -> Self {
        Self {
            kind: FailureKind::Infrastructure,
            detail: error.expose_detail().to_owned(),
        }
    }

    pub(crate) fn provider_session_mismatch() -> Self {
        Self {
            kind: FailureKind::Protocol,
            detail: "provider returned a session for a different runtime family".to_owned(),
        }
    }

    pub(crate) fn provider_panicked() -> Self {
        Self {
            kind: FailureKind::Infrastructure,
            detail: "provider executor unwound across the dispatch boundary".to_owned(),
        }
    }

    pub(crate) fn process_service(error: &ProcessServiceError) -> Self {
        let kind = match error.kind() {
            ProcessServiceErrorKind::Denied
            | ProcessServiceErrorKind::OutsideRoot
            | ProcessServiceErrorKind::NotEnrolled => FailureKind::Permission,
            ProcessServiceErrorKind::Unavailable
            | ProcessServiceErrorKind::InvalidRequest
            | ProcessServiceErrorKind::Spawn
            | ProcessServiceErrorKind::OutcomeIndeterminate => FailureKind::Infrastructure,
        };
        Self {
            kind,
            detail: error.expose_detail().to_owned(),
        }
    }

    pub(crate) fn effect_service(error: &EffectServiceError) -> Self {
        let kind = match error.kind() {
            EffectServiceErrorKind::Denied
            | EffectServiceErrorKind::OutsideRoot
            | EffectServiceErrorKind::ApprovalRequired => FailureKind::Permission,
            EffectServiceErrorKind::Unavailable
            | EffectServiceErrorKind::InvalidRequest
            | EffectServiceErrorKind::Execution
            | EffectServiceErrorKind::Cancelled
            | EffectServiceErrorKind::DeadlineExceeded => FailureKind::Infrastructure,
        };
        Self {
            kind,
            detail: error.expose_detail().to_owned(),
        }
    }

    pub(crate) fn supervisor_receipt(detail: &'static str) -> Self {
        Self {
            kind: FailureKind::Infrastructure,
            detail: detail.to_owned(),
        }
    }

    pub(crate) fn unsupported_capability(cap: RuntimeCap) -> Self {
        Self {
            kind: FailureKind::Protocol,
            detail: format!("runtime emitted unadvertised capability: {}", cap.as_str()),
        }
    }

    pub(crate) fn effect_receipt_mismatch() -> Self {
        Self {
            kind: FailureKind::Protocol,
            detail: "effect receipt idempotency key does not match the request".to_owned(),
        }
    }

    pub(crate) fn evidence_verification(error: &EvidenceVerifierError) -> Self {
        let kind = match error.kind() {
            EvidenceVerifierErrorKind::Missing
            | EvidenceVerifierErrorKind::Mismatch
            | EvidenceVerifierErrorKind::InvalidExpectation => FailureKind::Verification,
            EvidenceVerifierErrorKind::Denied => FailureKind::Permission,
            EvidenceVerifierErrorKind::Unavailable => FailureKind::Infrastructure,
        };
        Self {
            kind,
            detail: error.expose_detail().to_owned(),
        }
    }

    pub(crate) fn evidence_authority_missing() -> Self {
        Self {
            kind: FailureKind::Verification,
            detail:
                "completion supplied expected or claimed evidence without an independent verifier"
                    .to_owned(),
        }
    }

    pub(crate) fn process_exit(purpose: ProcessPurpose, status: ProcessExitStatus) -> Self {
        let kind = match purpose {
            ProcessPurpose::Verification => FailureKind::Verification,
            ProcessPurpose::Git => FailureKind::GitConflict,
            ProcessPurpose::ProviderWorker | ProcessPurpose::Tool | ProcessPurpose::Plugin => {
                FailureKind::Infrastructure
            }
        };
        Self {
            kind,
            detail: format!("{purpose:?} process returned non-success status {status:?}"),
        }
    }

    pub(crate) fn unacknowledged_process_truncation(purpose: ProcessPurpose) -> Self {
        Self {
            kind: match purpose {
                ProcessPurpose::Verification => FailureKind::Verification,
                ProcessPurpose::Git => FailureKind::GitConflict,
                ProcessPurpose::ProviderWorker | ProcessPurpose::Tool | ProcessPurpose::Plugin => {
                    FailureKind::Infrastructure
                }
            },
            detail: format!("{purpose:?} process discarded unacknowledged output"),
        }
    }

    pub(crate) fn event_protocol(detail: &'static str) -> Self {
        Self {
            kind: FailureKind::Protocol,
            detail: detail.to_owned(),
        }
    }

    pub(crate) fn effect_receipt_overflow() -> Self {
        Self {
            kind: FailureKind::Protocol,
            detail: "effect receipts exceeded the attempt evidence bound".to_owned(),
        }
    }

    pub(crate) fn authoritative_overflow(count: u64) -> Self {
        Self {
            kind: FailureKind::Infrastructure,
            detail: format!("{count} additional authoritative failures were coalesced"),
        }
    }

    pub(crate) fn progress_overflow(count: usize) -> Self {
        Self {
            kind: FailureKind::Protocol,
            detail: format!("{count} context progress receipts exceeded the merge bound"),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> FailureKind {
        self.kind
    }

    /// Explicitly exposes diagnostic detail. Keep it out of ordinary logs.
    #[must_use]
    pub fn expose_detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Debug for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Failure")
            .field("kind", &self.kind)
            .field("detail", &"[REDACTED]")
            .field("detail_len", &self.detail.len())
            .finish()
    }
}

/// How execution stopped at the mechanical layer.
///
/// There is deliberately no `Completed` variant. Completion is represented by
/// [`AttemptOutcome::Completed`], making `partial(Completed, ...)` impossible.
///
/// ```compile_fail
/// use orchestrator_exec::MechanicalTermination;
/// let _invalid = MechanicalTermination::Completed;
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MechanicalTermination {
    Cancelled,
    HardDeadlineExceeded,
    WatchdogStalled,
    ProcessExited(ProcessExitStatus),
    ProviderStreamEnded,
    SupervisorFailure,
    EventDeliveryFailure,
    ContractViolation,
}

/// Session, cost, tool, and artifact evidence shared by completed and partial
/// attempts.
#[derive(Clone, Default)]
pub struct AttemptEvidence {
    session: Option<SessionHandle>,
    cost: Option<CostInfo>,
    tool_observations: Vec<ToolObservation>,
    artifact_receipts: Vec<ArtifactReceipt>,
    effect_receipts: Vec<EffectReceipt>,
}

impl AttemptEvidence {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_session(mut self, session: SessionHandle) -> Self {
        self.session = Some(session);
        self
    }

    #[must_use]
    pub fn with_cost(mut self, cost: CostInfo) -> Self {
        self.cost = Some(cost);
        self
    }

    pub fn with_tool_observation(
        mut self,
        observation: ToolObservation,
    ) -> Result<Self, ContractError> {
        if self.tool_observations.len() >= MAX_TOOL_OBSERVATIONS {
            return Err(ContractError::TooManyItems {
                field: "tool_observations",
                max: MAX_TOOL_OBSERVATIONS,
            });
        }
        self.tool_observations.push(observation);
        Ok(self)
    }

    pub fn with_artifact_receipt(
        mut self,
        mut receipt: ArtifactReceipt,
    ) -> Result<Self, ContractError> {
        if self.artifact_receipts.len() >= MAX_ARTIFACT_RECEIPTS {
            return Err(ContractError::TooManyItems {
                field: "artifact_receipts",
                max: MAX_ARTIFACT_RECEIPTS,
            });
        }
        receipt.mark_unverified();
        self.artifact_receipts.push(receipt);
        Ok(self)
    }

    #[must_use]
    pub fn session(&self) -> Option<&SessionHandle> {
        self.session.as_ref()
    }

    #[must_use]
    pub fn cost(&self) -> Option<&CostInfo> {
        self.cost.as_ref()
    }

    #[must_use]
    pub fn tool_observations(&self) -> &[ToolObservation] {
        &self.tool_observations
    }

    #[must_use]
    pub fn artifact_receipts(&self) -> &[ArtifactReceipt] {
        &self.artifact_receipts
    }

    /// Durable receipts for effects admitted through the attempt context.
    #[must_use]
    pub fn effect_receipts(&self) -> &[EffectReceipt] {
        &self.effect_receipts
    }

    fn strip_session(&mut self) {
        self.session = None;
    }

    fn set_session(&mut self, session: SessionHandle) {
        self.session = Some(session);
    }

    fn set_cost(&mut self, cost: CostInfo) {
        self.cost = Some(cost);
    }

    fn push_tool_observation(&mut self, observation: ToolObservation) -> Result<(), ContractError> {
        if self.tool_observations.len() >= MAX_TOOL_OBSERVATIONS {
            return Err(ContractError::TooManyItems {
                field: "tool_observations",
                max: MAX_TOOL_OBSERVATIONS,
            });
        }
        self.tool_observations.push(observation);
        Ok(())
    }

    fn push_artifact_receipt(&mut self, mut receipt: ArtifactReceipt) -> Result<(), ContractError> {
        if self.artifact_receipts.len() >= MAX_ARTIFACT_RECEIPTS {
            return Err(ContractError::TooManyItems {
                field: "artifact_receipts",
                max: MAX_ARTIFACT_RECEIPTS,
            });
        }
        receipt.mark_unverified();
        self.artifact_receipts.push(receipt);
        Ok(())
    }

    fn push_effect_receipt(&mut self, receipt: EffectReceipt) -> Result<(), ContractError> {
        if self.effect_receipts.len() >= MAX_EFFECT_RECEIPTS {
            return Err(ContractError::TooManyItems {
                field: "effect_receipts",
                max: MAX_EFFECT_RECEIPTS,
            });
        }
        self.effect_receipts.push(receipt);
        Ok(())
    }

    fn replace_with_verified_artifacts(&mut self, mut receipts: Vec<ArtifactReceipt>) {
        for receipt in &mut receipts {
            receipt.mark_authority_verified();
        }
        self.artifact_receipts = receipts;
    }
}

impl fmt::Debug for AttemptEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttemptEvidence")
            .field("has_session", &self.session.is_some())
            .field("cost", &self.cost)
            .field("tool_observations", &self.tool_observations.len())
            .field("artifact_receipts", &self.artifact_receipts.len())
            .field("effect_receipts", &self.effect_receipts.len())
            .finish()
    }
}

/// Work gathered before a non-completed termination.
#[derive(Clone)]
pub struct PartialWork {
    partial_output: Option<String>,
    evidence: AttemptEvidence,
}

impl PartialWork {
    pub fn new(
        partial_output: Option<String>,
        evidence: AttemptEvidence,
    ) -> Result<Self, ContractError> {
        if let Some(output) = partial_output.as_deref() {
            validate_required_content("partial_output", output, MAX_FINAL_OUTPUT_BYTES)?;
        }
        Ok(Self {
            partial_output,
            evidence,
        })
    }

    #[must_use]
    pub fn empty() -> Self {
        Self {
            partial_output: None,
            evidence: AttemptEvidence::new(),
        }
    }

    #[must_use]
    pub fn partial_output(&self) -> Option<&str> {
        self.partial_output.as_deref()
    }

    #[must_use]
    pub fn evidence(&self) -> &AttemptEvidence {
        &self.evidence
    }
}

impl Default for PartialWork {
    fn default() -> Self {
        Self::empty()
    }
}

impl fmt::Debug for PartialWork {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PartialWork")
            .field("has_partial_output", &self.partial_output.is_some())
            .field(
                "partial_output_len",
                &self.partial_output.as_ref().map_or(0, String::len),
            )
            .field("evidence", &self.evidence)
            .finish()
    }
}

/// Valid completed attempt. Fields are private; construct through
/// [`AttemptOutcome::completed`].
pub struct CompletedAttempt {
    final_output: String,
    evidence: AttemptEvidence,
    elapsed: Duration,
}

impl CompletedAttempt {
    #[must_use]
    pub fn final_output(&self) -> &str {
        &self.final_output
    }

    #[must_use]
    pub fn evidence(&self) -> &AttemptEvidence {
        &self.evidence
    }

    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

impl fmt::Debug for CompletedAttempt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompletedAttempt")
            .field("final_output", &"[REDACTED]")
            .field("final_output_len", &self.final_output.len())
            .field("evidence", &self.evidence)
            .field("elapsed", &self.elapsed)
            .finish()
    }
}

/// Valid non-completed attempt. Fields are private; construct through
/// [`AttemptOutcome::incomplete`].
pub struct IncompleteAttempt {
    termination: MechanicalTermination,
    failures: Vec<Failure>,
    partial_work: PartialWork,
    elapsed: Duration,
}

impl IncompleteAttempt {
    #[must_use]
    pub const fn termination(&self) -> MechanicalTermination {
        self.termination
    }

    #[must_use]
    pub fn failures(&self) -> &[Failure] {
        &self.failures
    }

    #[must_use]
    pub fn partial_work(&self) -> &PartialWork {
        &self.partial_work
    }

    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

impl fmt::Debug for IncompleteAttempt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IncompleteAttempt")
            .field("termination", &self.termination)
            .field("failure_kinds", &FailureKinds(&self.failures))
            .field("partial_work", &self.partial_work)
            .field("elapsed", &self.elapsed)
            .finish()
    }
}

struct FailureKinds<'a>(&'a [Failure]);

impl fmt::Debug for FailureKinds<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_list()
            .entries(self.0.iter().map(Failure::kind))
            .finish()
    }
}

/// Always-returned executor outcome. Completion and incomplete work are
/// separate variants whose inner fields are private.
pub enum AttemptOutcome {
    Completed(CompletedAttempt),
    Incomplete(IncompleteAttempt),
}

impl AttemptOutcome {
    pub fn completed(
        final_output: impl Into<String>,
        evidence: AttemptEvidence,
        elapsed: Duration,
    ) -> Result<Self, ContractError> {
        let final_output = final_output.into();
        validate_required_content("final_output", &final_output, MAX_FINAL_OUTPUT_BYTES)?;
        Ok(Self::Completed(CompletedAttempt {
            final_output,
            evidence,
            elapsed,
        }))
    }

    #[must_use]
    pub fn incomplete(
        termination: MechanicalTermination,
        failure: Option<Failure>,
        partial_work: PartialWork,
        elapsed: Duration,
    ) -> Self {
        let failures = failure.into_iter().collect();
        Self::Incomplete(IncompleteAttempt {
            termination,
            failures,
            partial_work,
            elapsed,
        })
    }

    #[must_use]
    pub const fn is_completed(&self) -> bool {
        matches!(self, Self::Completed(_))
    }

    #[must_use]
    pub fn output(&self) -> Option<&str> {
        match self {
            Self::Completed(completed) => Some(completed.final_output()),
            Self::Incomplete(incomplete) => incomplete.partial_work().partial_output(),
        }
    }

    #[must_use]
    pub fn evidence(&self) -> &AttemptEvidence {
        match self {
            Self::Completed(completed) => completed.evidence(),
            Self::Incomplete(incomplete) => incomplete.partial_work().evidence(),
        }
    }

    #[must_use]
    pub fn termination(&self) -> Option<MechanicalTermination> {
        match self {
            Self::Completed(_) => None,
            Self::Incomplete(incomplete) => Some(incomplete.termination()),
        }
    }

    #[must_use]
    pub fn failures(&self) -> &[Failure] {
        match self {
            Self::Completed(_) => &[],
            Self::Incomplete(incomplete) => incomplete.failures(),
        }
    }

    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        match self {
            Self::Completed(completed) => completed.elapsed(),
            Self::Incomplete(incomplete) => incomplete.elapsed(),
        }
    }

    pub(crate) fn returned_session_matches(&self, runtime: &RuntimeFamily) -> bool {
        self.evidence()
            .session()
            .is_none_or(|session| session.can_resume_into(runtime))
    }

    pub(crate) fn reject_provider_session(self) -> Self {
        self.terminate_authoritatively(
            MechanicalTermination::ContractViolation,
            Some(Failure::provider_session_mismatch()),
            true,
        )
    }

    pub(crate) fn reject_capability(self, cap: RuntimeCap) -> Self {
        self.terminate_authoritatively(
            MechanicalTermination::ContractViolation,
            Some(Failure::unsupported_capability(cap)),
            matches!(cap, RuntimeCap::SessionResume),
        )
    }

    pub(crate) fn reject_authoritative(
        self,
        termination: MechanicalTermination,
        failure: Option<Failure>,
    ) -> Self {
        self.terminate_authoritatively(termination, failure, false)
    }

    pub(crate) fn provider_panic(partial_work: PartialWork, elapsed: Duration) -> Self {
        Self::Incomplete(IncompleteAttempt {
            termination: MechanicalTermination::SupervisorFailure,
            failures: vec![Failure::provider_panicked()],
            partial_work,
            elapsed,
        })
    }

    pub(crate) fn reconcile_progress(mut self, progress: PartialWork) -> Self {
        let PartialWork {
            partial_output,
            evidence,
        } = progress;
        let overflow = match &mut self {
            Self::Completed(completed) => merge_context_evidence(&mut completed.evidence, evidence),
            Self::Incomplete(incomplete) => {
                if partial_output.is_some() {
                    incomplete.partial_work.partial_output = partial_output;
                }
                merge_context_evidence(&mut incomplete.partial_work.evidence, evidence)
            }
        };
        if overflow > 0 {
            self = self.terminate_authoritatively(
                MechanicalTermination::ContractViolation,
                Some(Failure::progress_overflow(overflow)),
                false,
            );
        }
        self
    }

    pub(crate) fn demote_provider_artifacts(mut self) -> Self {
        let artifacts = match &mut self {
            Self::Completed(completed) => &mut completed.evidence.artifact_receipts,
            Self::Incomplete(incomplete) => &mut incomplete.partial_work.evidence.artifact_receipts,
        };
        for artifact in artifacts {
            artifact.mark_unverified();
        }
        self
    }

    fn terminate_authoritatively(
        self,
        completed_termination: MechanicalTermination,
        failure: Option<Failure>,
        strip_session: bool,
    ) -> Self {
        match self {
            Self::Completed(completed) => {
                let mut evidence = completed.evidence;
                if strip_session {
                    evidence.strip_session();
                }
                Self::Incomplete(IncompleteAttempt {
                    termination: completed_termination,
                    failures: failure.into_iter().collect(),
                    partial_work: PartialWork {
                        partial_output: Some(completed.final_output),
                        evidence,
                    },
                    elapsed: completed.elapsed,
                })
            }
            Self::Incomplete(mut incomplete) => {
                if strip_session {
                    incomplete.partial_work.evidence.strip_session();
                }
                if termination_precedence(completed_termination)
                    >= termination_precedence(incomplete.termination)
                {
                    incomplete.termination = completed_termination;
                }
                if let Some(failure) = failure {
                    incomplete.failures.push(failure);
                }
                Self::Incomplete(incomplete)
            }
        }
    }
}

fn merge_context_evidence(target: &mut AttemptEvidence, context: AttemptEvidence) -> usize {
    if context.session.is_some() {
        target.session = context.session;
    }
    if context.cost.is_some() {
        target.cost = context.cost;
    }

    let mut overflow = 0;
    let mut tools = context.tool_observations;
    for observation in std::mem::take(&mut target.tool_observations) {
        if tools.contains(&observation) {
            continue;
        }
        if tools.len() < MAX_TOOL_OBSERVATIONS {
            tools.push(observation);
        } else {
            overflow += 1;
        }
    }
    target.tool_observations = tools;

    let mut artifacts = context.artifact_receipts;
    for receipt in std::mem::take(&mut target.artifact_receipts) {
        if artifacts.contains(&receipt) {
            continue;
        }
        if artifacts.len() < MAX_ARTIFACT_RECEIPTS {
            artifacts.push(receipt);
        } else {
            overflow += 1;
        }
    }
    target.artifact_receipts = artifacts;

    let mut effects = context.effect_receipts;
    for receipt in std::mem::take(&mut target.effect_receipts) {
        if effects.contains(&receipt) {
            continue;
        }
        if effects.len() < MAX_EFFECT_RECEIPTS {
            effects.push(receipt);
        } else {
            overflow += 1;
        }
    }
    target.effect_receipts = effects;
    overflow
}

/// Explicit precedence for independently authoritative terminal observations.
/// Contract and persistence failures outrank supervisor/mechanical state; a
/// deadline outranks cancellation only when both are independently observed.
const fn termination_precedence(termination: MechanicalTermination) -> u8 {
    match termination {
        MechanicalTermination::ContractViolation => 8,
        MechanicalTermination::EventDeliveryFailure => 7,
        MechanicalTermination::SupervisorFailure => 6,
        MechanicalTermination::HardDeadlineExceeded => 5,
        MechanicalTermination::Cancelled => 4,
        MechanicalTermination::WatchdogStalled => 3,
        MechanicalTermination::ProcessExited(_) => 2,
        MechanicalTermination::ProviderStreamEnded => 1,
    }
}

impl fmt::Debug for AttemptOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Completed(completed) => formatter
                .debug_tuple("AttemptOutcome::Completed")
                .field(completed)
                .finish(),
            Self::Incomplete(incomplete) => formatter
                .debug_tuple("AttemptOutcome::Incomplete")
                .field(incomplete)
                .finish(),
        }
    }
}

struct AuthoritativeFailure {
    termination: MechanicalTermination,
    failure: Option<Failure>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EventLifecycle {
    AwaitingSpawn,
    Active,
    Terminal(WorkerEventKind),
}

/// Per-attempt execution services and progress accumulator.
///
/// The process service is also the cancellation source, so the composition
/// root cannot accidentally give execution classification one token and child
/// supervision another. This context is single-dispatch and owns the worker
/// identity attached to every event. Every dispatched request has already
/// proven that its worker-spawn metadata is representable. Pre-provider
/// cancellation and deadline outcomes still open and terminally close the
/// event stream, so durable lifecycle admission cannot be left incomplete.
pub struct ExecutionContext<'a> {
    process_service: &'a dyn ProcessService,
    clock: &'a dyn Clock,
    watchdog: &'a dyn WatchdogPolicy,
    effect_service: &'a dyn EffectService,
    evidence_verifier: Option<&'a dyn EvidenceVerifier>,
    hard_deadline: Instant,
    verbose: bool,
    event_sink: &'a mut dyn EventSink,
    worker_identity: WorkerIdentity,
    event_spawn: Option<WorkerSpawned>,
    event_lifecycle: EventLifecycle,
    progress: PartialWork,
    authoritative_failures: Vec<AuthoritativeFailure>,
    authoritative_overflow: u64,
    observed_streaming: bool,
    started_at: Instant,
    dispatched: bool,
    attempt: Option<u32>,
}

impl<'a> ExecutionContext<'a> {
    #[must_use]
    pub fn new(
        process_service: &'a dyn ProcessService,
        clock: &'a dyn Clock,
        watchdog: &'a dyn WatchdogPolicy,
        effect_service: &'a dyn EffectService,
        event_sink: &'a mut dyn EventSink,
        worker_identity: WorkerIdentity,
        hard_deadline: Instant,
    ) -> Self {
        let started_at = clock.now();
        Self {
            process_service,
            clock,
            watchdog,
            effect_service,
            evidence_verifier: None,
            hard_deadline,
            verbose: false,
            event_sink,
            worker_identity,
            event_spawn: None,
            event_lifecycle: EventLifecycle::AwaitingSpawn,
            progress: PartialWork::empty(),
            authoritative_failures: Vec::new(),
            authoritative_overflow: 0,
            observed_streaming: false,
            started_at,
            dispatched: false,
            attempt: None,
        }
    }

    #[must_use]
    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    /// Selects the independent evidence authority used to qualify completion.
    #[must_use]
    pub fn with_evidence_verifier(mut self, verifier: &'a dyn EvidenceVerifier) -> Self {
        self.evidence_verifier = Some(verifier);
        self
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.process_service.is_cancelled()
    }

    #[must_use]
    pub const fn hard_deadline(&self) -> Instant {
        self.hard_deadline
    }

    #[must_use]
    pub fn now(&self) -> Instant {
        self.clock.now()
    }

    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.hard_deadline
            .saturating_duration_since(self.clock.now())
    }

    #[must_use]
    pub fn watchdog_decision(&self, last_activity: Instant) -> WatchdogDecision {
        self.watchdog.evaluate(self.clock.now(), last_activity)
    }

    #[must_use]
    pub fn stall_window(&self) -> Duration {
        self.watchdog.stall_window()
    }

    /// Performs the process operation through the attempt-bound service. Fatal
    /// supervisor receipts and service errors are remembered even if an
    /// executor ignores the returned `Result` or receipt.
    pub fn run_process(
        &mut self,
        request: &ProcessRequest,
    ) -> Result<ProcessReceipt, ProcessServiceError> {
        if let Some(reason) = self.current_process_preflight_reason() {
            let (termination, receipt_termination) = match reason {
                ProcessPreflightReason::Cancelled => (
                    MechanicalTermination::Cancelled,
                    ProcessTerminationReceipt::Cancelled,
                ),
                ProcessPreflightReason::DeadlineExceeded => (
                    MechanicalTermination::HardDeadlineExceeded,
                    ProcessTerminationReceipt::DeadlineExceeded,
                ),
            };
            let preflight = match ProcessPreflight::new(self.process_service, request, reason) {
                Ok(preflight) => preflight,
                Err(error) => {
                    self.remember_authoritative(
                        MechanicalTermination::SupervisorFailure,
                        Some(Failure::process_service(&error)),
                    );
                    self.remember_current_attempt_termination();
                    return Err(error);
                }
            };
            if let Err(error) = self.process_service.finish_preflight(request, preflight) {
                self.remember_authoritative(
                    MechanicalTermination::SupervisorFailure,
                    Some(Failure::process_service(&error)),
                );
                self.remember_current_attempt_termination();
                return Err(error);
            }
            self.remember_authoritative(termination, None);
            return Ok(ProcessReceipt::preflight(receipt_termination));
        }
        let budget = ProcessBudget::new(
            self.hard_deadline,
            self.remaining(),
            self.watchdog.stall_window(),
        );
        match self.process_service.execute(request, budget) {
            Ok(receipt) => {
                self.observe_process_receipt(request, &receipt);
                self.remember_current_attempt_termination();
                Ok(receipt)
            }
            Err(error) => {
                self.remember_authoritative(
                    MechanicalTermination::SupervisorFailure,
                    Some(Failure::process_service(&error)),
                );
                self.remember_current_attempt_termination();
                Err(error)
            }
        }
    }

    /// Performs an effect and requires a receipt whose idempotency key matches
    /// the request. Service and receipt failures remain authoritative state.
    pub fn run_effect(
        &mut self,
        request: &EffectRequest,
    ) -> Result<EffectReceipt, EffectServiceError> {
        if let Some(termination) = self.current_attempt_termination() {
            self.remember_authoritative(termination, None);
            return Err(match termination {
                MechanicalTermination::Cancelled => EffectServiceError::cancelled(),
                MechanicalTermination::HardDeadlineExceeded => {
                    EffectServiceError::deadline_exceeded()
                }
                _ => EffectServiceError::deadline_exceeded(),
            });
        }
        let budget = EffectBudget::new(
            self.process_service,
            self.clock,
            self.hard_deadline,
            self.remaining(),
        );
        match self.effect_service.execute(request, budget) {
            Ok(receipt) => {
                if receipt.idempotency_key() != request.idempotency_key() {
                    self.remember_authoritative(
                        MechanicalTermination::ContractViolation,
                        Some(Failure::effect_receipt_mismatch()),
                    );
                } else if self
                    .progress
                    .evidence
                    .push_effect_receipt(receipt.clone())
                    .is_err()
                {
                    self.remember_authoritative(
                        MechanicalTermination::ContractViolation,
                        Some(Failure::effect_receipt_overflow()),
                    );
                }
                self.remember_current_attempt_termination();
                Ok(receipt)
            }
            Err(error) => {
                match error.kind() {
                    EffectServiceErrorKind::Cancelled => {
                        self.remember_authoritative(MechanicalTermination::Cancelled, None);
                    }
                    EffectServiceErrorKind::DeadlineExceeded => {
                        self.remember_authoritative(
                            MechanicalTermination::HardDeadlineExceeded,
                            None,
                        );
                    }
                    _ => self.remember_authoritative(
                        MechanicalTermination::SupervisorFailure,
                        Some(Failure::effect_service(&error)),
                    ),
                }
                self.remember_current_attempt_termination();
                Err(error)
            }
        }
    }

    #[must_use]
    pub const fn verbose(&self) -> bool {
        self.verbose
    }

    /// Emits provider output inside the context-owned event lifecycle.
    /// Spawn and terminal events are derived centrally from the bound request
    /// and final outcome, so providers cannot persist contradictory terminals.
    pub fn emit(&mut self, payload: &WorkerEventPayload) -> Result<EventReceipt, EventSinkError> {
        if !matches!(payload, WorkerEventPayload::Output(_)) {
            return Err(self.reject_event_protocol(
                "providers may emit worker.output only; lifecycle events are context owned",
            ));
        }
        self.observed_streaming |= payload.requires_streaming_capability();
        self.ensure_spawned()?;
        if self.event_lifecycle != EventLifecycle::Active {
            return Err(self.reject_event_protocol("worker.output was emitted after terminal"));
        }
        self.commit_event(payload)
    }

    fn commit_event(
        &mut self,
        payload: &WorkerEventPayload,
    ) -> Result<EventReceipt, EventSinkError> {
        let Some(attempt) = self.attempt else {
            return Err(self
                .reject_event_protocol("event stream was used before execution attempt binding"));
        };
        let draft = WorkerEventDraft::new(&self.worker_identity, attempt, payload);
        match self.event_sink.emit(&draft) {
            Ok(receipt) => Ok(receipt),
            Err(error) => {
                self.remember_authoritative(
                    MechanicalTermination::EventDeliveryFailure,
                    Some(Failure::event_delivery(&error)),
                );
                Err(error)
            }
        }
    }

    fn reject_event_protocol(&mut self, detail: &'static str) -> EventSinkError {
        self.remember_authoritative(
            MechanicalTermination::ContractViolation,
            Some(Failure::event_protocol(detail)),
        );
        EventSinkError::rejected(detail)
    }

    fn ensure_spawned(&mut self) -> Result<(), EventSinkError> {
        match self.event_lifecycle {
            EventLifecycle::AwaitingSpawn => {
                let Some(spawned) = self.event_spawn.clone() else {
                    return Err(
                        self.reject_event_protocol("event stream was used before request binding")
                    );
                };
                let payload = WorkerEventPayload::Spawned(spawned);
                self.commit_event(&payload)?;
                self.event_lifecycle = EventLifecycle::Active;
                Ok(())
            }
            EventLifecycle::Active => Ok(()),
            EventLifecycle::Terminal(_) => {
                Err(self.reject_event_protocol("event stream already reached terminal"))
            }
        }
    }

    pub(crate) fn start_event_stream(&mut self) -> Result<(), EventSinkError> {
        self.ensure_spawned()
    }

    /// Replaces the context-owned partial output snapshot. Providers should call
    /// this before operations that may unwind.
    pub fn record_partial_output(
        &mut self,
        output: impl Into<String>,
    ) -> Result<(), ContractError> {
        let output = output.into();
        validate_required_content("partial_output", &output, MAX_FINAL_OUTPUT_BYTES)?;
        self.progress.partial_output = Some(output);
        Ok(())
    }

    pub fn record_session(&mut self, session: SessionHandle) {
        self.progress.evidence.set_session(session);
    }

    pub fn record_cost(&mut self, cost: CostInfo) {
        self.progress.evidence.set_cost(cost);
    }

    pub fn record_tool_observation(
        &mut self,
        observation: ToolObservation,
    ) -> Result<(), ContractError> {
        self.progress.evidence.push_tool_observation(observation)
    }

    pub fn record_artifact_receipt(
        &mut self,
        receipt: ArtifactReceipt,
    ) -> Result<(), ContractError> {
        self.progress.evidence.push_artifact_receipt(receipt)
    }

    #[must_use]
    pub fn worker_identity(&self) -> &WorkerIdentity {
        &self.worker_identity
    }

    pub(crate) fn bind_event_stream(&mut self, request: &ExecutionRequest) {
        self.event_spawn = Some(request.worker_spawned());
    }

    pub(crate) fn qualify_completion_evidence(
        &mut self,
        request: &ExecutionRequest,
        mut outcome: AttemptOutcome,
    ) -> AttemptOutcome {
        let AttemptOutcome::Completed(completed) = &mut outcome else {
            return outcome;
        };
        if request.expected_evidence().is_empty() && completed.evidence.artifact_receipts.is_empty()
        {
            return outcome;
        }
        let Some(verifier) = self.evidence_verifier else {
            return outcome.reject_authoritative(
                MechanicalTermination::ContractViolation,
                Some(Failure::evidence_authority_missing()),
            );
        };
        let verification_request = EvidenceVerificationRequest::new(
            request.mission(),
            request.phase(),
            request.attempt(),
            request.worker_dir(),
            request.target_dir(),
            request.expected_evidence(),
            &completed.evidence.artifact_receipts,
        );
        match verifier.verify(&verification_request) {
            Ok(verification) => {
                completed
                    .evidence
                    .replace_with_verified_artifacts(verification.into_artifacts());
                outcome
            }
            Err(error) => {
                let termination = match error.kind() {
                    EvidenceVerifierErrorKind::Missing
                    | EvidenceVerifierErrorKind::Mismatch
                    | EvidenceVerifierErrorKind::InvalidExpectation => {
                        MechanicalTermination::ContractViolation
                    }
                    EvidenceVerifierErrorKind::Denied | EvidenceVerifierErrorKind::Unavailable => {
                        MechanicalTermination::SupervisorFailure
                    }
                };
                outcome
                    .reject_authoritative(termination, Some(Failure::evidence_verification(&error)))
            }
        }
    }

    pub(crate) fn emit_terminal(&mut self, outcome: &AttemptOutcome) -> Result<(), EventSinkError> {
        self.ensure_spawned()?;
        if self.event_lifecycle != EventLifecycle::Active {
            return Err(
                self.reject_event_protocol("attempt attempted more than one worker terminal event")
            );
        }
        let payload = terminal_payload(outcome).map_err(|_| {
            self.reject_event_protocol("attempt outcome cannot be represented as a worker event")
        })?;
        let kind = payload.kind();
        self.commit_event(&payload)?;
        self.event_lifecycle = EventLifecycle::Terminal(kind);
        Ok(())
    }

    fn observe_process_receipt(&mut self, request: &ProcessRequest, receipt: &ProcessReceipt) {
        match receipt.termination() {
            ProcessTerminationReceipt::Exited(status) => {
                if status.as_code() != Some(0) {
                    self.remember_authoritative(
                        MechanicalTermination::ProcessExited(status),
                        Some(Failure::process_exit(request.purpose(), status)),
                    );
                }
            }
            ProcessTerminationReceipt::Cancelled => {
                self.remember_authoritative(MechanicalTermination::Cancelled, None);
            }
            ProcessTerminationReceipt::DeadlineExceeded => {
                self.remember_authoritative(MechanicalTermination::HardDeadlineExceeded, None);
            }
            ProcessTerminationReceipt::Stalled => {
                self.remember_authoritative(MechanicalTermination::WatchdogStalled, None);
            }
            ProcessTerminationReceipt::OutputLimit => {
                self.remember_authoritative(
                    MechanicalTermination::SupervisorFailure,
                    Some(Failure::supervisor_receipt(
                        "process exceeded its bounded output receipt",
                    )),
                );
            }
            ProcessTerminationReceipt::SupervisorFailure => {
                self.remember_authoritative(
                    MechanicalTermination::SupervisorFailure,
                    Some(Failure::supervisor_receipt(
                        "process supervisor reported infrastructure failure",
                    )),
                );
            }
            ProcessTerminationReceipt::UnresolvedOwnership => {
                self.remember_authoritative(
                    MechanicalTermination::SupervisorFailure,
                    Some(Failure::supervisor_receipt(
                        "process supervisor could not prove ownership cleanup",
                    )),
                );
            }
        }
        if receipt.expose_stdout().len() > request.max_output_bytes()
            || receipt.expose_stderr().len() > request.max_output_bytes()
        {
            self.remember_authoritative(
                MechanicalTermination::SupervisorFailure,
                Some(Failure::supervisor_receipt(
                    "process receipt exceeded the request output bound",
                )),
            );
        }
        if receipt.truncated_output_acknowledged() && !request.truncated_output_acknowledged() {
            self.remember_authoritative(
                MechanicalTermination::SupervisorFailure,
                Some(Failure::supervisor_receipt(
                    "process receipt exceeded the request truncation policy",
                )),
            );
        }
        if (receipt.stdout_discarded() > 0 || receipt.stderr_discarded() > 0)
            && !receipt.truncated_output_acknowledged()
        {
            self.remember_authoritative(
                MechanicalTermination::SupervisorFailure,
                Some(Failure::unacknowledged_process_truncation(
                    request.purpose(),
                )),
            );
        }
        if !receipt.ownership_released()
            && !matches!(
                receipt.termination(),
                ProcessTerminationReceipt::UnresolvedOwnership
            )
        {
            self.remember_authoritative(
                MechanicalTermination::SupervisorFailure,
                Some(Failure::supervisor_receipt(
                    "process receipt did not prove ownership release",
                )),
            );
        }
    }

    fn remember_authoritative(
        &mut self,
        termination: MechanicalTermination,
        failure: Option<Failure>,
    ) {
        if self.authoritative_failures.len() < MAX_AUTHORITATIVE_FAILURES {
            self.authoritative_failures.push(AuthoritativeFailure {
                termination,
                failure,
            });
        } else {
            self.authoritative_overflow = self.authoritative_overflow.saturating_add(1);
        }
    }

    fn current_attempt_termination(&self) -> Option<MechanicalTermination> {
        if self.is_cancelled() {
            Some(MechanicalTermination::Cancelled)
        } else if self.remaining().is_zero() {
            Some(MechanicalTermination::HardDeadlineExceeded)
        } else {
            None
        }
    }

    fn current_process_preflight_reason(&self) -> Option<ProcessPreflightReason> {
        if self.is_cancelled() {
            Some(ProcessPreflightReason::Cancelled)
        } else if self.remaining().is_zero() {
            Some(ProcessPreflightReason::DeadlineExceeded)
        } else {
            None
        }
    }

    fn remember_current_attempt_termination(&mut self) {
        if let Some(termination) = self.current_attempt_termination() {
            self.remember_authoritative(termination, None);
        }
    }

    pub(crate) fn preflight_outcome(&self) -> Option<AttemptOutcome> {
        self.current_attempt_termination().map(|termination| {
            AttemptOutcome::incomplete(
                termination,
                None,
                self.progress.clone(),
                self.clock.now().saturating_duration_since(self.started_at),
            )
        })
    }

    pub(crate) fn finalize_attempt_state(&mut self) {
        self.remember_current_attempt_termination();
    }

    /// Closes a dispatch that must not invoke its provider.
    ///
    /// A definitely uncommitted first spawn may be retried only as part of this
    /// one terminal closure attempt. Indeterminate publication is handled by
    /// the caller without another emit. A persistently unavailable sink remains
    /// an explicit event-delivery failure for the enclosing lifecycle.
    pub(crate) fn finish_without_provider(
        &mut self,
        mut outcome: AttemptOutcome,
    ) -> AttemptOutcome {
        self.finalize_attempt_state();
        outcome = self.apply_authoritative_failures(outcome);
        let _terminal = self.emit_terminal(&outcome);
        self.apply_authoritative_failures(outcome)
    }

    pub(crate) fn identity_matches(&self, request: &ExecutionRequest) -> bool {
        self.worker_identity.is_live()
            && self.worker_identity.mission_id() == request.mission()
            && self.worker_identity.phase_id() == request.phase()
    }

    pub(crate) fn begin_dispatch(&mut self, attempt: u32) -> bool {
        if self.dispatched {
            return false;
        }
        self.dispatched = true;
        self.attempt = Some(attempt);
        true
    }

    pub(crate) const fn observed_streaming(&self) -> bool {
        self.observed_streaming
    }

    pub(crate) fn panic_outcome(&self) -> AttemptOutcome {
        AttemptOutcome::provider_panic(
            self.progress.clone(),
            self.clock.now().saturating_duration_since(self.started_at),
        )
    }

    pub(crate) fn progress_snapshot(&self) -> PartialWork {
        self.progress.clone()
    }

    pub(crate) fn apply_authoritative_failures(
        &mut self,
        mut outcome: AttemptOutcome,
    ) -> AttemptOutcome {
        for authoritative in std::mem::take(&mut self.authoritative_failures) {
            outcome =
                outcome.reject_authoritative(authoritative.termination, authoritative.failure);
        }
        if self.authoritative_overflow > 0 {
            let count = std::mem::take(&mut self.authoritative_overflow);
            outcome = outcome.reject_authoritative(
                MechanicalTermination::ContractViolation,
                Some(Failure::authoritative_overflow(count)),
            );
        }
        outcome
    }
}

fn terminal_payload(outcome: &AttemptOutcome) -> Result<WorkerEventPayload, WorkerEventError> {
    let duration = format_event_duration(outcome.elapsed());
    match outcome {
        AttemptOutcome::Completed(completed) => Ok(WorkerEventPayload::Completed(
            WorkerCompleted::new(completed.final_output().len(), duration)?,
        )),
        AttemptOutcome::Incomplete(incomplete) => {
            let error = incomplete.failures().first().map_or_else(
                || format!("attempt ended: {:?}", incomplete.termination()),
                |failure| failure.expose_detail().to_owned(),
            );
            let exit_code = match incomplete.termination() {
                MechanicalTermination::ProcessExited(status) => status.as_code(),
                _ => None,
            };
            Ok(WorkerEventPayload::Failed(WorkerFailed::new(
                WorkerFailedFields {
                    error,
                    duration: Some(duration),
                    output_len: incomplete.partial_work().partial_output().map(str::len),
                    exit_code,
                    stderr_tail: None,
                },
            )?))
        }
    }
}

fn format_event_duration(duration: Duration) -> String {
    let nanos = duration.as_nanos();
    if nanos == 0 {
        "0s".to_owned()
    } else if nanos % 1_000_000_000 == 0 {
        format!("{}s", nanos / 1_000_000_000)
    } else {
        format!("{nanos}ns")
    }
}

impl fmt::Debug for ExecutionContext<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionContext")
            .field("is_cancelled", &self.is_cancelled())
            .field("hard_deadline", &self.hard_deadline)
            .field("verbose", &self.verbose)
            .field("has_evidence_verifier", &self.evidence_verifier.is_some())
            .field("worker_identity", &self.worker_identity)
            .field("event_lifecycle", &self.event_lifecycle)
            .field("progress", &self.progress)
            .field("authoritative_failures", &self.authoritative_failures.len())
            .field("authoritative_overflow", &self.authoritative_overflow)
            .field("observed_streaming", &self.observed_streaming)
            .field("dispatched", &self.dispatched)
            .field("attempt", &self.attempt)
            .finish_non_exhaustive()
    }
}

/// Runtime capability flag validated before dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeCap {
    ToolUse,
    SessionResume,
    Streaming,
    CostReport,
    Artifacts,
}

impl RuntimeCap {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ToolUse => "tool_use",
            Self::SessionResume => "session_resume",
            Self::Streaming => "streaming",
            Self::CostReport => "cost_report",
            Self::Artifacts => "artifacts",
        }
    }
}

/// Capability set advertised by a runtime.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeCaps {
    pub tool_use: bool,
    pub session_resume: bool,
    pub streaming: bool,
    pub cost_report: bool,
    pub artifacts: bool,
}

impl RuntimeCaps {
    #[must_use]
    pub const fn supports(self, cap: RuntimeCap) -> bool {
        match cap {
            RuntimeCap::ToolUse => self.tool_use,
            RuntimeCap::SessionResume => self.session_resume,
            RuntimeCap::Streaming => self.streaming,
            RuntimeCap::CostReport => self.cost_report,
            RuntimeCap::Artifacts => self.artifacts,
        }
    }

    #[must_use]
    pub fn advertised(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.tool_use {
            out.push("tool_use");
        }
        if self.session_resume {
            out.push("session_resume");
        }
        if self.streaming {
            out.push("streaming");
        }
        if self.cost_report {
            out.push("cost_report");
        }
        if self.artifacts {
            out.push("artifacts");
        }
        out
    }
}

/// Optional runtime identity and capability advertisement.
#[derive(Clone, Debug)]
pub struct RuntimeDescriptor {
    runtime: RuntimeFamily,
    caps: RuntimeCaps,
}

impl RuntimeDescriptor {
    #[must_use]
    pub const fn new(runtime: RuntimeFamily, caps: RuntimeCaps) -> Self {
        Self { runtime, caps }
    }

    #[must_use]
    pub fn runtime(&self) -> &RuntimeFamily {
        &self.runtime
    }

    #[must_use]
    pub const fn caps(&self) -> RuntimeCaps {
        self.caps
    }

    #[must_use]
    pub const fn supports(&self, cap: RuntimeCap) -> bool {
        self.caps.supports(cap)
    }
}

/// Registry-bound request supplied to executor implementations. External code
/// can inspect but cannot construct it, preventing direct cross-runtime calls.
pub struct DispatchRequest<'a> {
    request: &'a ExecutionRequest,
    requested_runtime: &'a RuntimeFamily,
    effective_runtime: &'a RuntimeFamily,
}

impl<'a> DispatchRequest<'a> {
    pub(crate) const fn new(
        request: &'a ExecutionRequest,
        requested_runtime: &'a RuntimeFamily,
        effective_runtime: &'a RuntimeFamily,
    ) -> Self {
        Self {
            request,
            requested_runtime,
            effective_runtime,
        }
    }

    #[must_use]
    pub const fn request(&self) -> &'a ExecutionRequest {
        self.request
    }

    #[must_use]
    pub const fn requested_runtime(&self) -> &'a RuntimeFamily {
        self.requested_runtime
    }

    #[must_use]
    pub const fn effective_runtime(&self) -> &'a RuntimeFamily {
        self.effective_runtime
    }
}

impl fmt::Debug for DispatchRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DispatchRequest")
            .field("request", &self.request)
            .field("requested_runtime", &self.requested_runtime)
            .field("effective_runtime", &self.effective_runtime)
            .finish()
    }
}

/// Runtime backend contract. The registry is the only producer of
/// [`DispatchRequest`], so implementations cannot be invoked with an unbound
/// request through this trait.
pub trait PhaseExecutor: Send + Sync {
    fn execute(
        &self,
        request: DispatchRequest<'_>,
        context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome;

    /// Returns a descriptor when the backend advertises one, matching Go's
    /// optional runtime-description behavior.
    fn descriptor(&self) -> Option<RuntimeDescriptor>;
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ContractError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} exceeds the {max} byte contract limit")]
    TooLong { field: &'static str, max: usize },
    #[error("{field} contains disallowed control characters")]
    ControlCharacter { field: &'static str },
    #[error("{field} must be one-based")]
    ZeroOrdinal { field: &'static str },
    #[error("{field} exceeds the {max} item contract limit")]
    TooManyItems { field: &'static str, max: usize },
    #[error("{field} exceeds the {max} value contract limit")]
    ValueTooLarge { field: &'static str, max: u64 },
    #[error("{field} contains a NUL byte")]
    Nul { field: &'static str },
    #[error("runtime family has invalid syntax")]
    InvalidRuntimeFamily,
    #[error("resume session runtime does not match the effective request runtime")]
    SessionRuntimeMismatch,
    #[error("cost must be finite and non-negative")]
    InvalidCost,
    #[error("artifact path must be a normalized relative path")]
    UnnormalizedArtifactPath,
}

fn worker_spawned_contract_error(error: WorkerEventError) -> ContractError {
    match error {
        WorkerEventError::Empty { field } => ContractError::Empty { field },
        WorkerEventError::TooLong { field, max } => ContractError::TooLong { field, max },
        WorkerEventError::ControlCharacter { field } => ContractError::ControlCharacter { field },
        // `WorkerSpawned::new` cannot produce payload, receipt, or attempt
        // errors. Keep the conversion total without widening the public
        // `ContractError` enum if that constructor gains stricter invariants.
        WorkerEventError::EmptyPayload => ContractError::Empty {
            field: "worker_spawned",
        },
        WorkerEventError::InvalidEventId => ContractError::ControlCharacter {
            field: "worker_spawned.id",
        },
        WorkerEventError::InvalidTimestamp => ContractError::ControlCharacter {
            field: "worker_spawned.timestamp",
        },
        WorkerEventError::InvalidSequence => ContractError::ZeroOrdinal {
            field: "worker_spawned.sequence",
        },
        WorkerEventError::InvalidAttempt => ContractError::ZeroOrdinal {
            field: "worker_spawned.attempt",
        },
    }
}

fn validate_list(field: &'static str, values: &[String]) -> Result<(), ContractError> {
    if values.len() > MAX_REQUEST_LIST_ITEMS {
        return Err(ContractError::TooManyItems {
            field,
            max: MAX_REQUEST_LIST_ITEMS,
        });
    }
    for value in values {
        validate_required_content(field, value, MAX_LABEL_BYTES)?;
    }
    Ok(())
}

fn validate_required_label(
    field: &'static str,
    value: &str,
    max: usize,
) -> Result<(), ContractError> {
    if value.is_empty() {
        return Err(ContractError::Empty { field });
    }
    if value.chars().any(char::is_control) {
        return Err(ContractError::ControlCharacter { field });
    }
    validate_length(field, value, max)
}

fn validate_required_content(
    field: &'static str,
    value: &str,
    max: usize,
) -> Result<(), ContractError> {
    if value.is_empty() {
        return Err(ContractError::Empty { field });
    }
    validate_content(field, value, max)
}

fn validate_content(field: &'static str, value: &str, max: usize) -> Result<(), ContractError> {
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(ContractError::ControlCharacter { field });
    }
    validate_length(field, value, max)
}

fn validate_length(field: &'static str, value: &str, max: usize) -> Result<(), ContractError> {
    if value.len() > max {
        return Err(ContractError::TooLong { field, max });
    }
    Ok(())
}

fn validate_path(field: &'static str, path: &Path) -> Result<(), ContractError> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.is_empty() {
        return Err(ContractError::Empty { field });
    }
    if bytes.len() > MAX_PATH_BYTES {
        return Err(ContractError::TooLong {
            field,
            max: MAX_PATH_BYTES,
        });
    }
    if bytes.contains(&0) {
        return Err(ContractError::Nul { field });
    }
    Ok(())
}
