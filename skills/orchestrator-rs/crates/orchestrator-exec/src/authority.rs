//! Attempt-bound execution services supplied by the composition root.
//!
//! These traits perform operations; they are not advisory checks followed by
//! an unrestricted operation. A production [`ProcessService`] is expected to
//! own the same wakeable cancellation token observed through [`Cancellation`]
//! and to translate [`ProcessBudget`] into the process supervisor's deadline.
//! This is a capability boundary for cooperating Rust code, not a sandbox: code
//! linked into the process can still call `std::process` or operating-system
//! APIs directly and must remain trusted.

use std::fmt;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::contract::ArtifactReceipt;

const MAX_SERVICE_DETAIL_BYTES: usize = 4 * 1024;
const MAX_EXECUTABLE_ID_BYTES: usize = 512;
const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_ARGUMENTS: usize = 128;
const MAX_ARGUMENT_BYTES: usize = 8 * 1024;
const MAX_ARGUMENT_TOTAL_BYTES: usize = 64 * 1024;
const MAX_ENVIRONMENT_ENTRIES: usize = 64;
const MAX_ENVIRONMENT_TOTAL_BYTES: usize = 64 * 1024;
const MAX_STDIN_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROCESS_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_PROCESS_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_EFFECT_RESOURCE_BYTES: usize = 4 * 1024;
const MAX_EFFECT_ID_BYTES: usize = 256;
const MAX_EFFECT_INPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_EFFECT_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_VERIFIED_ARTIFACTS: usize = 4_096;
const PROCESS_REQUEST_FINGERPRINT_DOMAIN: &[u8] =
    b"nanika.orchestrator.process-request-fingerprint.v1\0";

/// Read-only view of the wakeable cancellation token owned by a process
/// service. Cancellation is monotonic.
pub trait Cancellation: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

/// Monotonic time source injected for deterministic deadline handling.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// Read-only evidence-verification input assembled by the execution context.
///
/// Artifact receipts in `claimed_artifacts` are untrusted provider claims. A
/// verifier must independently inspect its admitted filesystem, process, or
/// persistence authorities rather than accepting those fields as proof.
pub struct EvidenceVerificationRequest<'a> {
    mission_id: &'a str,
    phase_id: &'a str,
    attempt: u32,
    worker_root: &'a Path,
    target_root: Option<&'a Path>,
    expected: &'a [String],
    claimed_artifacts: &'a [ArtifactReceipt],
}

impl<'a> EvidenceVerificationRequest<'a> {
    pub(crate) const fn new(
        mission_id: &'a str,
        phase_id: &'a str,
        attempt: u32,
        worker_root: &'a Path,
        target_root: Option<&'a Path>,
        expected: &'a [String],
        claimed_artifacts: &'a [ArtifactReceipt],
    ) -> Self {
        Self {
            mission_id,
            phase_id,
            attempt,
            worker_root,
            target_root,
            expected,
            claimed_artifacts,
        }
    }

    #[must_use]
    pub const fn mission_id(&self) -> &str {
        self.mission_id
    }

    #[must_use]
    pub const fn phase_id(&self) -> &str {
        self.phase_id
    }

    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    #[must_use]
    pub const fn worker_root(&self) -> &Path {
        self.worker_root
    }

    #[must_use]
    pub const fn target_root(&self) -> Option<&Path> {
        self.target_root
    }

    #[must_use]
    pub const fn expected(&self) -> &[String] {
        self.expected
    }

    #[must_use]
    pub const fn claimed_artifacts(&self) -> &[ArtifactReceipt] {
        self.claimed_artifacts
    }
}

impl fmt::Debug for EvidenceVerificationRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvidenceVerificationRequest")
            .field("mission_id_len", &self.mission_id.len())
            .field("phase_id_len", &self.phase_id.len())
            .field("attempt", &self.attempt)
            .field("worker_root", &"[REDACTED]")
            .field("has_target_root", &self.target_root.is_some())
            .field("expected_count", &self.expected.len())
            .field("claimed_artifact_count", &self.claimed_artifacts.len())
            .finish()
    }
}

/// Evidence independently attested by a trusted composition-root authority.
pub struct EvidenceVerification {
    artifacts: Vec<ArtifactReceipt>,
}

impl EvidenceVerification {
    pub fn new(artifacts: Vec<ArtifactReceipt>) -> Result<Self, ServiceContractError> {
        if artifacts.len() > MAX_VERIFIED_ARTIFACTS {
            return Err(ServiceContractError::TooManyItems {
                field: "verified_artifacts",
                max: MAX_VERIFIED_ARTIFACTS,
            });
        }
        Ok(Self { artifacts })
    }

    pub(crate) fn into_artifacts(self) -> Vec<ArtifactReceipt> {
        self.artifacts
    }
}

impl fmt::Debug for EvidenceVerification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvidenceVerification")
            .field("artifact_count", &self.artifacts.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceVerifierErrorKind {
    Missing,
    Mismatch,
    Denied,
    Unavailable,
    InvalidExpectation,
}

#[derive(Clone, Error)]
#[error("evidence verification failed ({kind:?})")]
pub struct EvidenceVerifierError {
    kind: EvidenceVerifierErrorKind,
    detail: String,
}

impl EvidenceVerifierError {
    pub fn new(
        kind: EvidenceVerifierErrorKind,
        detail: impl Into<String>,
    ) -> Result<Self, ServiceContractError> {
        let detail = detail.into();
        validate_label(
            "evidence_verifier_detail",
            &detail,
            MAX_SERVICE_DETAIL_BYTES,
        )?;
        Ok(Self { kind, detail })
    }

    #[must_use]
    pub const fn kind(&self) -> EvidenceVerifierErrorKind {
        self.kind
    }

    #[must_use]
    pub fn expose_detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Debug for EvidenceVerifierError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvidenceVerifierError")
            .field("kind", &self.kind)
            .field("detail", &"[REDACTED]")
            .field("detail_len", &self.detail.len())
            .finish()
    }
}

/// Independently proves requested evidence at the authority boundary.
pub trait EvidenceVerifier: Send + Sync {
    fn verify(
        &self,
        request: &EvidenceVerificationRequest<'_>,
    ) -> Result<EvidenceVerification, EvidenceVerifierError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchdogDecision {
    Continue { next_check: Instant },
    Stalled,
}

pub trait WatchdogPolicy: Send + Sync {
    fn evaluate(&self, now: Instant, last_activity: Instant) -> WatchdogDecision;
    fn stall_window(&self) -> Duration;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessPurpose {
    ProviderWorker,
    Git,
    Tool,
    Plugin,
    Verification,
}

/// Stable digest of every persisted semantic field in one [`ProcessRequest`].
///
/// The digest is deliberately opaque and its `Debug` implementation never
/// reveals request material. It is suitable for binding a durable outbox
/// intent to the exact request later presented at the process start gate.
///
/// [`ProcessBudget`] is intentionally excluded. Its `Instant` deadline is a
/// monotonic, boot-local value and cannot be serialized across a restart. A
/// claimed and authorized attempt is therefore one-shot: it is never released
/// again after restart, and any retry receives a new claim attempt and budget.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct ProcessRequestFingerprint([u8; 32]);

impl ProcessRequestFingerprint {
    /// Returns the fixed-width digest for conversion into an opaque process
    /// gate binding. The bytes contain no executable, path, argument, stdin,
    /// or environment authority.
    #[must_use]
    pub const fn into_bytes(self) -> [u8; 32] {
        self.0
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for ProcessRequestFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProcessRequestFingerprint([REDACTED])")
    }
}

/// Complete bounded input for one process operation.
///
/// `executable_id` is resolved by the concrete service to an enrolled binary;
/// it is deliberately not an arbitrary executable path. The child environment
/// is explicit and is expected to start empty in the concrete adapter.
pub struct ProcessRequest {
    purpose: ProcessPurpose,
    executable_id: String,
    working_root: PathBuf,
    arguments: Vec<String>,
    environment: Vec<(String, String)>,
    stdin: Option<Vec<u8>>,
    max_output_bytes: usize,
    truncated_output_acknowledged: bool,
}

impl ProcessRequest {
    pub fn new(
        purpose: ProcessPurpose,
        executable_id: impl Into<String>,
        working_root: impl Into<PathBuf>,
    ) -> Result<Self, ServiceContractError> {
        let executable_id = executable_id.into();
        validate_label("executable_id", &executable_id, MAX_EXECUTABLE_ID_BYTES)?;
        let working_root = working_root.into();
        validate_path("working_root", &working_root)?;
        Ok(Self {
            purpose,
            executable_id,
            working_root,
            arguments: Vec::new(),
            environment: Vec::new(),
            stdin: None,
            max_output_bytes: DEFAULT_PROCESS_OUTPUT_BYTES,
            truncated_output_acknowledged: false,
        })
    }

    pub fn with_argument(
        mut self,
        argument: impl Into<String>,
    ) -> Result<Self, ServiceContractError> {
        let argument = argument.into();
        validate_process_value("argument", &argument, MAX_ARGUMENT_BYTES)?;
        if self.arguments.len() >= MAX_ARGUMENTS {
            return Err(ServiceContractError::TooManyItems {
                field: "arguments",
                max: MAX_ARGUMENTS,
            });
        }
        let total = self
            .arguments
            .iter()
            .map(String::len)
            .sum::<usize>()
            .saturating_add(argument.len());
        if total > MAX_ARGUMENT_TOTAL_BYTES {
            return Err(ServiceContractError::TooLong {
                field: "arguments",
                max: MAX_ARGUMENT_TOTAL_BYTES,
            });
        }
        self.arguments.push(argument);
        Ok(self)
    }

    pub fn with_environment(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, ServiceContractError> {
        let key = key.into();
        let value = value.into();
        validate_environment_key(&key)?;
        validate_process_value("environment_value", &value, MAX_ARGUMENT_TOTAL_BYTES)?;
        if self.environment.len() >= MAX_ENVIRONMENT_ENTRIES {
            return Err(ServiceContractError::TooManyItems {
                field: "environment",
                max: MAX_ENVIRONMENT_ENTRIES,
            });
        }
        let total = self
            .environment
            .iter()
            .map(|(key, value)| key.len().saturating_add(value.len()))
            .sum::<usize>()
            .saturating_add(key.len())
            .saturating_add(value.len());
        if total > MAX_ENVIRONMENT_TOTAL_BYTES {
            return Err(ServiceContractError::TooLong {
                field: "environment",
                max: MAX_ENVIRONMENT_TOTAL_BYTES,
            });
        }
        self.environment.push((key, value));
        Ok(self)
    }

    pub fn with_stdin(mut self, stdin: Vec<u8>) -> Result<Self, ServiceContractError> {
        if stdin.len() > MAX_STDIN_BYTES {
            return Err(ServiceContractError::TooLong {
                field: "stdin",
                max: MAX_STDIN_BYTES,
            });
        }
        self.stdin = Some(stdin);
        Ok(self)
    }

    pub fn with_max_output_bytes(mut self, max: usize) -> Result<Self, ServiceContractError> {
        if max == 0 {
            return Err(ServiceContractError::Empty {
                field: "max_output_bytes",
            });
        }
        if max > MAX_PROCESS_OUTPUT_BYTES {
            return Err(ServiceContractError::TooLong {
                field: "max_output_bytes",
                max: MAX_PROCESS_OUTPUT_BYTES,
            });
        }
        self.max_output_bytes = max;
        Ok(self)
    }

    /// Acknowledges that this operation does not require complete
    /// stdout/stderr. `OutputLimit` termination remains fatal; this applies
    /// only to a clean exit receipt that reports discarded bytes.
    #[must_use]
    pub const fn with_truncated_output_acknowledged(mut self) -> Self {
        self.truncated_output_acknowledged = true;
        self
    }

    #[must_use]
    pub const fn purpose(&self) -> ProcessPurpose {
        self.purpose
    }

    #[must_use]
    pub fn executable_id(&self) -> &str {
        &self.executable_id
    }

    #[must_use]
    pub fn working_root(&self) -> &Path {
        &self.working_root
    }

    /// Exposes argv tail only to the operation-performing adapter.
    #[must_use]
    pub fn expose_arguments(&self) -> &[String] {
        &self.arguments
    }

    /// Exposes explicit environment values only to the operation adapter.
    #[must_use]
    pub fn expose_environment(&self) -> &[(String, String)] {
        &self.environment
    }

    #[must_use]
    pub fn expose_stdin(&self) -> Option<&[u8]> {
        self.stdin.as_deref()
    }

    #[must_use]
    pub const fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }

    #[must_use]
    pub const fn truncated_output_acknowledged(&self) -> bool {
        self.truncated_output_acknowledged
    }

    /// Computes a domain-separated, fixed-order, length-prefixed digest of
    /// this request's exact semantics.
    ///
    /// Ordering of arguments and environment pairs is significant. `None`
    /// stdin remains distinct from an explicitly present empty byte string.
    /// Paths are hashed as their native Unix bytes, without lossy Unicode
    /// conversion. See [`ProcessRequestFingerprint`] for the budget/restart
    /// invariant.
    #[cfg(unix)]
    #[must_use]
    pub fn fingerprint(&self) -> ProcessRequestFingerprint {
        let mut digest = Sha256::new();
        digest.update(PROCESS_REQUEST_FINGERPRINT_DOMAIN);
        digest_field(
            &mut digest,
            b"purpose",
            &[process_purpose_tag(self.purpose)],
        );
        digest_field(&mut digest, b"executable_id", self.executable_id.as_bytes());
        digest_field(
            &mut digest,
            b"working_root",
            self.working_root.as_os_str().as_bytes(),
        );
        digest_sequence(
            &mut digest,
            b"arguments",
            self.arguments.iter().map(String::as_bytes),
        );
        digest.update((b"environment".len() as u64).to_be_bytes());
        digest.update(b"environment");
        digest.update((self.environment.len() as u64).to_be_bytes());
        for (key, value) in &self.environment {
            digest_field(&mut digest, b"key", key.as_bytes());
            digest_field(&mut digest, b"value", value.as_bytes());
        }
        match self.stdin.as_deref() {
            None => digest_field(&mut digest, b"stdin_presence", &[0]),
            Some(stdin) => {
                digest_field(&mut digest, b"stdin_presence", &[1]);
                digest_field(&mut digest, b"stdin", stdin);
            }
        }
        digest_field(
            &mut digest,
            b"max_output_bytes",
            &(self.max_output_bytes as u64).to_be_bytes(),
        );
        digest_field(
            &mut digest,
            b"truncated_output_acknowledged",
            &[u8::from(self.truncated_output_acknowledged)],
        );
        ProcessRequestFingerprint(digest.finalize().into())
    }
}

const fn process_purpose_tag(purpose: ProcessPurpose) -> u8 {
    match purpose {
        ProcessPurpose::ProviderWorker => 1,
        ProcessPurpose::Git => 2,
        ProcessPurpose::Tool => 3,
        ProcessPurpose::Plugin => 4,
        ProcessPurpose::Verification => 5,
    }
}

fn digest_field(digest: &mut Sha256, name: &[u8], value: &[u8]) {
    digest.update((name.len() as u64).to_be_bytes());
    digest.update(name);
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn digest_sequence<'a>(digest: &mut Sha256, name: &[u8], values: impl Iterator<Item = &'a [u8]>) {
    let values = values.collect::<Vec<_>>();
    digest.update((name.len() as u64).to_be_bytes());
    digest.update(name);
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        digest_field(digest, b"item", value);
    }
}

impl fmt::Debug for ProcessRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessRequest")
            .field("purpose", &self.purpose)
            .field("executable_id_len", &self.executable_id.len())
            .field("working_root", &"[REDACTED]")
            .field("argument_count", &self.arguments.len())
            .field("environment_count", &self.environment.len())
            .field("stdin_len", &self.stdin.as_ref().map_or(0, Vec::len))
            .field("max_output_bytes", &self.max_output_bytes)
            .field(
                "truncated_output_acknowledged",
                &self.truncated_output_acknowledged,
            )
            .finish()
    }
}

/// Exact attempt budget passed by [`crate::ExecutionContext`] to the service.
/// The service itself owns the cancellation source through the supertrait.
#[derive(Clone, Copy, Debug)]
pub struct ProcessBudget {
    hard_deadline: Instant,
    remaining: Duration,
    stall_window: Duration,
}

/// Finite reason why an execution context closed a process operation before
/// dispatching it to [`ProcessService::execute`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessPreflightReason {
    /// The attempt's shared cancellation source was already cancelled.
    Cancelled,
    /// The attempt's hard deadline had already elapsed.
    DeadlineExceeded,
}

/// One-use proof that [`crate::ExecutionContext`] observed a finite
/// pre-dispatch process outcome for one exact request and process-service
/// capability.
///
/// The token is deliberately non-cloneable and has no public constructor. A
/// process service may inspect its bounded reason and must reject it when
/// [`Self::bind`] does not match the service and request being completed.
///
/// ```compile_fail
/// use orchestrator_exec::{
///     ProcessPreflight, ProcessPreflightReason, ProcessRequest, ProcessService,
/// };
///
/// fn forge(service: &dyn ProcessService, request: &ProcessRequest) {
///     let _ = ProcessPreflight::new(
///         service,
///         request,
///         ProcessPreflightReason::Cancelled,
///     );
/// }
/// ```
///
/// ```compile_fail
/// use orchestrator_exec::ProcessPreflight;
///
/// fn require_clone<T: Clone>() {}
/// require_clone::<ProcessPreflight<'static>>();
/// ```
#[must_use = "a process preflight proof must be consumed by its process service"]
pub struct ProcessPreflight<'service> {
    reason: ProcessPreflightReason,
    request_fingerprint: ProcessRequestFingerprint,
    service_identity: process_service_capability::ExactIdentity,
    _service_borrow: &'service dyn ProcessService,
}

impl<'service> ProcessPreflight<'service> {
    pub(crate) fn new(
        service: &'service dyn ProcessService,
        request: &ProcessRequest,
        reason: ProcessPreflightReason,
    ) -> Result<Self, ProcessServiceError> {
        let Some(service_identity) =
            process_service_capability::Identity::exact_process_service_identity(service)
        else {
            return Err(ProcessServiceError::zero_sized_capability());
        };
        Ok(Self {
            reason,
            request_fingerprint: request.fingerprint(),
            service_identity,
            _service_borrow: service,
        })
    }

    /// Consumes this proof and binds it to this exact service capability and
    /// `request`'s exact semantics. A mismatch consumes the proof and returns
    /// `None`, so it cannot be tried against another service.
    #[must_use]
    pub fn bind(
        self,
        service: &dyn ProcessService,
        request: &ProcessRequest,
    ) -> Option<BoundProcessPreflight> {
        let service_identity =
            process_service_capability::Identity::exact_process_service_identity(service)?;
        (self.service_identity == service_identity
            && self.request_fingerprint == request.fingerprint())
        .then_some(BoundProcessPreflight {
            reason: self.reason,
            request_fingerprint: self.request_fingerprint,
        })
    }
}

impl fmt::Debug for ProcessPreflight<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessPreflight")
            .field("reason", &self.reason)
            .field("request_fingerprint", &"[REDACTED]")
            .field("service_identity", &"[REDACTED]")
            .finish()
    }
}

/// Service-validated one-use process preflight proof carried across a private
/// service boundary. It remains request-bound and non-cloneable.
#[must_use = "a bound process preflight proof must be terminally completed"]
pub struct BoundProcessPreflight {
    reason: ProcessPreflightReason,
    request_fingerprint: ProcessRequestFingerprint,
}

impl BoundProcessPreflight {
    /// Returns the finite pre-dispatch outcome observed by the context.
    #[must_use]
    pub const fn reason(&self) -> ProcessPreflightReason {
        self.reason
    }

    /// Revalidates the exact request after it crosses a private actor boundary.
    #[must_use]
    pub fn matches_request(&self, request: &ProcessRequest) -> bool {
        self.request_fingerprint == request.fingerprint()
    }
}

impl fmt::Debug for BoundProcessPreflight {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundProcessPreflight")
            .field("reason", &self.reason)
            .field("request_fingerprint", &"[REDACTED]")
            .finish()
    }
}

impl ProcessBudget {
    /// Creates a process budget from an absolute hard deadline, the remaining
    /// duration, and a stall-detection window. Exposed `pub` so the canary
    /// composition can construct a budget without a full `ExecutionContext`.
    #[must_use]
    pub const fn new(hard_deadline: Instant, remaining: Duration, stall_window: Duration) -> Self {
        Self {
            hard_deadline,
            remaining,
            stall_window,
        }
    }

    #[must_use]
    pub const fn hard_deadline(self) -> Instant {
        self.hard_deadline
    }

    #[must_use]
    pub const fn remaining(self) -> Duration {
        self.remaining
    }

    /// Output-inactivity window selected by the attempt's watchdog policy.
    #[must_use]
    pub const fn stall_window(self) -> Duration {
        self.stall_window
    }
}

/// Exact shared cancellation/deadline view for one effect operation.
#[derive(Clone, Copy)]
pub struct EffectBudget<'a> {
    process_service: &'a dyn ProcessService,
    clock: &'a dyn Clock,
    hard_deadline: Instant,
    remaining_at_dispatch: Duration,
}

impl<'a> EffectBudget<'a> {
    pub(crate) fn new(
        process_service: &'a dyn ProcessService,
        clock: &'a dyn Clock,
        hard_deadline: Instant,
        remaining_at_dispatch: Duration,
    ) -> Self {
        Self {
            process_service,
            clock,
            hard_deadline,
            remaining_at_dispatch,
        }
    }

    /// Linearizes admission immediately before the first irreversible commit.
    /// A service must not apply a new effect when this returns `Err`.
    pub fn admit(self) -> Result<EffectAdmission, EffectServiceError> {
        if self.process_service.is_cancelled() {
            return Err(EffectServiceError::cancelled());
        }
        let admitted_at = self.clock.now();
        if admitted_at >= self.hard_deadline {
            return Err(EffectServiceError::deadline_exceeded());
        }
        Ok(EffectAdmission {
            admitted_at,
            hard_deadline: self.hard_deadline,
        })
    }

    #[must_use]
    pub fn is_cancelled(self) -> bool {
        self.process_service.is_cancelled()
    }

    #[must_use]
    pub fn now(self) -> Instant {
        self.clock.now()
    }

    #[must_use]
    pub const fn hard_deadline(self) -> Instant {
        self.hard_deadline
    }

    #[must_use]
    pub const fn remaining_at_dispatch(self) -> Duration {
        self.remaining_at_dispatch
    }

    #[must_use]
    pub fn remaining(self) -> Duration {
        self.hard_deadline
            .saturating_duration_since(self.clock.now())
    }
}

impl fmt::Debug for EffectBudget<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectBudget")
            .field("is_cancelled", &self.process_service.is_cancelled())
            .field("hard_deadline", &self.hard_deadline)
            .field("remaining_at_dispatch", &self.remaining_at_dispatch)
            .finish()
    }
}

/// Successful effect admission snapshot. Cancellation after this admission may
/// race an in-flight commit; a committed effect must still return its durable
/// receipt so retry reconciliation can observe it.
#[derive(Clone, Copy, Debug)]
pub struct EffectAdmission {
    admitted_at: Instant,
    hard_deadline: Instant,
}

impl EffectAdmission {
    #[must_use]
    pub const fn admitted_at(self) -> Instant {
        self.admitted_at
    }

    #[must_use]
    pub const fn hard_deadline(self) -> Instant {
        self.hard_deadline
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ProcessExitStatus(ProcessExitStatusKind);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessExitStatusKind {
    Code(i32),
    Signal(i32),
}

impl ProcessExitStatus {
    pub fn code(code: i32) -> Result<Self, ServiceContractError> {
        if code < 0 {
            return Err(ServiceContractError::InvalidExitStatus);
        }
        Ok(Self(ProcessExitStatusKind::Code(code)))
    }

    pub fn signal(signal: i32) -> Result<Self, ServiceContractError> {
        if signal <= 0 {
            return Err(ServiceContractError::InvalidExitStatus);
        }
        Ok(Self(ProcessExitStatusKind::Signal(signal)))
    }

    #[must_use]
    pub const fn as_code(self) -> Option<i32> {
        match self.0 {
            ProcessExitStatusKind::Code(code) => Some(code),
            ProcessExitStatusKind::Signal(_) => None,
        }
    }

    #[must_use]
    pub const fn as_signal(self) -> Option<i32> {
        match self.0 {
            ProcessExitStatusKind::Code(_) => None,
            ProcessExitStatusKind::Signal(signal) => Some(signal),
        }
    }
}

impl fmt::Debug for ProcessExitStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            ProcessExitStatusKind::Code(code) => {
                formatter.debug_tuple("Code").field(&code).finish()
            }
            ProcessExitStatusKind::Signal(signal) => {
                formatter.debug_tuple("Signal").field(&signal).finish()
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessTerminationReceipt {
    Exited(ProcessExitStatus),
    Cancelled,
    DeadlineExceeded,
    Stalled,
    OutputLimit,
    SupervisorFailure,
    UnresolvedOwnership,
}

/// Bounded evidence produced by an operation-performing process service.
#[derive(Clone)]
pub struct ProcessReceipt {
    termination: ProcessTerminationReceipt,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_discarded: u64,
    stderr_discarded: u64,
    truncated_output_acknowledged: bool,
    ownership_released: bool,
    elapsed: Duration,
}

impl ProcessReceipt {
    pub fn new(
        termination: ProcessTerminationReceipt,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        stdout_discarded: u64,
        stderr_discarded: u64,
        ownership_released: bool,
        elapsed: Duration,
    ) -> Result<Self, ServiceContractError> {
        validate_bytes("stdout", &stdout, MAX_PROCESS_OUTPUT_BYTES)?;
        validate_bytes("stderr", &stderr, MAX_PROCESS_OUTPUT_BYTES)?;
        Ok(Self {
            termination,
            stdout,
            stderr,
            stdout_discarded,
            stderr_discarded,
            truncated_output_acknowledged: false,
            ownership_released,
            elapsed,
        })
    }

    pub(crate) fn preflight(termination: ProcessTerminationReceipt) -> Self {
        Self {
            termination,
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_discarded: 0,
            stderr_discarded: 0,
            truncated_output_acknowledged: false,
            ownership_released: true,
            elapsed: Duration::ZERO,
        }
    }

    #[must_use]
    pub const fn termination(&self) -> ProcessTerminationReceipt {
        self.termination
    }

    #[must_use]
    pub fn expose_stdout(&self) -> &[u8] {
        &self.stdout
    }

    #[must_use]
    pub fn expose_stderr(&self) -> &[u8] {
        &self.stderr
    }

    #[must_use]
    pub const fn stdout_discarded(&self) -> u64 {
        self.stdout_discarded
    }

    #[must_use]
    pub const fn stderr_discarded(&self) -> u64 {
        self.stderr_discarded
    }

    /// Marks discarded output as an explicit part of the service receipt.
    /// The execution context still verifies that the exact request granted the
    /// same policy; a service cannot widen caller authority with this bit.
    #[must_use]
    pub const fn with_truncated_output_acknowledged(mut self) -> Self {
        self.truncated_output_acknowledged = true;
        self
    }

    #[must_use]
    pub const fn truncated_output_acknowledged(&self) -> bool {
        self.truncated_output_acknowledged
    }

    #[must_use]
    pub const fn ownership_released(&self) -> bool {
        self.ownership_released
    }

    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(
            self.termination,
            ProcessTerminationReceipt::Exited(status) if status.as_code() == Some(0)
        ) && self.ownership_released
            && ((self.stdout_discarded == 0 && self.stderr_discarded == 0)
                || self.truncated_output_acknowledged)
    }
}

impl fmt::Debug for ProcessReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessReceipt")
            .field("termination", &self.termination)
            .field("stdout_len", &self.stdout.len())
            .field("stderr_len", &self.stderr.len())
            .field("stdout_discarded", &self.stdout_discarded)
            .field("stderr_discarded", &self.stderr_discarded)
            .field(
                "truncated_output_acknowledged",
                &self.truncated_output_acknowledged,
            )
            .field("ownership_released", &self.ownership_released)
            .field("elapsed", &self.elapsed)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessServiceErrorKind {
    Denied,
    Unavailable,
    OutsideRoot,
    NotEnrolled,
    InvalidRequest,
    Spawn,
    OutcomeIndeterminate,
}

#[derive(Clone, Error)]
#[error("process service failed ({kind:?})")]
pub struct ProcessServiceError {
    kind: ProcessServiceErrorKind,
    detail: String,
}

impl ProcessServiceError {
    fn zero_sized_capability() -> Self {
        Self {
            kind: ProcessServiceErrorKind::InvalidRequest,
            detail: "zero-sized process service capabilities are unsupported".to_owned(),
        }
    }

    pub fn new(
        kind: ProcessServiceErrorKind,
        detail: impl Into<String>,
    ) -> Result<Self, ServiceContractError> {
        let detail = detail.into();
        validate_label("process_service_detail", &detail, MAX_SERVICE_DETAIL_BYTES)?;
        Ok(Self { kind, detail })
    }

    #[must_use]
    pub const fn kind(&self) -> ProcessServiceErrorKind {
        self.kind
    }

    #[must_use]
    pub fn expose_detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Debug for ProcessServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessServiceError")
            .field("kind", &self.kind)
            .field("detail", &"[REDACTED]")
            .field("detail_len", &self.detail.len())
            .finish()
    }
}

mod process_service_capability {
    use std::any::TypeId;

    #[derive(Clone, Copy, Eq, PartialEq)]
    pub struct ExactIdentity {
        concrete_type: TypeId,
        address: usize,
    }

    /// This blanket implementation makes the identity non-overridable by
    /// service implementations. Pairing the concrete type with the
    /// lifetime-pinned instance address distinguishes transparent wrappers
    /// without relying on vtable pointer uniqueness across codegen units.
    pub trait Identity {
        fn exact_process_service_identity(&self) -> Option<ExactIdentity>;
    }

    impl<T: 'static> Identity for T {
        fn exact_process_service_identity(&self) -> Option<ExactIdentity> {
            if std::mem::size_of::<T>() == 0 {
                return None;
            }
            Some(ExactIdentity {
                concrete_type: TypeId::of::<T>(),
                address: std::ptr::from_ref(self).cast::<()>() as usize,
            })
        }
    }
}

/// Performs a process operation under the context's exact deadline. The same
/// object supplies cancellation observation, preventing a context from wiring
/// one token for classification and a different token for child supervision.
/// Implementations are process-lifetime capabilities so their private exact
/// instance identity cannot borrow a replaceable wrapper.
pub trait ProcessService:
    process_service_capability::Identity + Cancellation + Send + Sync + 'static
{
    /// Completes service-owned state when the execution context proves that no
    /// process may be dispatched. Every implementation must explicitly consume
    /// the proof so a stateful service cannot silently strand durable work.
    fn finish_preflight(
        &self,
        request: &ProcessRequest,
        preflight: ProcessPreflight<'_>,
    ) -> Result<(), ProcessServiceError>;

    fn execute(
        &self,
        request: &ProcessRequest,
        budget: ProcessBudget,
    ) -> Result<ProcessReceipt, ProcessServiceError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectKind {
    FileRead,
    FileWrite,
    ArtifactWrite,
    NetworkRequest,
    GitMutation,
    PluginAction,
}

/// Complete bounded request for a non-process effect.
pub struct EffectRequest {
    kind: EffectKind,
    resource: String,
    idempotency_key: String,
    input: Option<Vec<u8>>,
}

impl EffectRequest {
    pub fn new(
        kind: EffectKind,
        resource: impl Into<String>,
        idempotency_key: impl Into<String>,
    ) -> Result<Self, ServiceContractError> {
        let resource = resource.into();
        let idempotency_key = idempotency_key.into();
        validate_label("resource", &resource, MAX_EFFECT_RESOURCE_BYTES)?;
        validate_label("idempotency_key", &idempotency_key, MAX_EFFECT_ID_BYTES)?;
        Ok(Self {
            kind,
            resource,
            idempotency_key,
            input: None,
        })
    }

    pub fn with_input(mut self, input: Vec<u8>) -> Result<Self, ServiceContractError> {
        validate_bytes("effect_input", &input, MAX_EFFECT_INPUT_BYTES)?;
        self.input = Some(input);
        Ok(self)
    }

    #[must_use]
    pub const fn kind(&self) -> EffectKind {
        self.kind
    }

    #[must_use]
    pub fn resource(&self) -> &str {
        &self.resource
    }

    #[must_use]
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    #[must_use]
    pub fn expose_input(&self) -> Option<&[u8]> {
        self.input.as_deref()
    }
}

impl fmt::Debug for EffectRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectRequest")
            .field("kind", &self.kind)
            .field("resource", &"[REDACTED]")
            .field("resource_len", &self.resource.len())
            .field("idempotency_key", &"[REDACTED]")
            .field("input_len", &self.input.as_ref().map_or(0, Vec::len))
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectStatus {
    Applied,
    AlreadyApplied,
    ReadOnly,
}

/// Durable acknowledgement returned by an effect service. Durable outbox
/// reconciliation is intentionally deferred to the persistence stage; an
/// executor may never report an effect without this receipt.
#[derive(Clone, Eq, PartialEq)]
pub struct EffectReceipt {
    operation_id: String,
    idempotency_key: String,
    status: EffectStatus,
    output: Option<Vec<u8>>,
}

impl EffectReceipt {
    pub fn new(
        operation_id: impl Into<String>,
        idempotency_key: impl Into<String>,
        status: EffectStatus,
        output: Option<Vec<u8>>,
    ) -> Result<Self, ServiceContractError> {
        let operation_id = operation_id.into();
        let idempotency_key = idempotency_key.into();
        validate_label("operation_id", &operation_id, MAX_EFFECT_ID_BYTES)?;
        validate_label("idempotency_key", &idempotency_key, MAX_EFFECT_ID_BYTES)?;
        if let Some(output) = output.as_deref() {
            validate_bytes("effect_output", output, MAX_EFFECT_OUTPUT_BYTES)?;
        }
        Ok(Self {
            operation_id,
            idempotency_key,
            status,
            output,
        })
    }

    #[must_use]
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    #[must_use]
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    #[must_use]
    pub const fn status(&self) -> EffectStatus {
        self.status
    }

    #[must_use]
    pub fn expose_output(&self) -> Option<&[u8]> {
        self.output.as_deref()
    }
}

impl fmt::Debug for EffectReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectReceipt")
            .field("operation_id_len", &self.operation_id.len())
            .field("idempotency_key", &"[REDACTED]")
            .field("status", &self.status)
            .field("output_len", &self.output.as_ref().map_or(0, Vec::len))
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectServiceErrorKind {
    Denied,
    Unavailable,
    OutsideRoot,
    ApprovalRequired,
    InvalidRequest,
    Execution,
    Cancelled,
    DeadlineExceeded,
}

#[derive(Clone, Error)]
#[error("effect service failed ({kind:?})")]
pub struct EffectServiceError {
    kind: EffectServiceErrorKind,
    detail: String,
}

impl EffectServiceError {
    pub fn new(
        kind: EffectServiceErrorKind,
        detail: impl Into<String>,
    ) -> Result<Self, ServiceContractError> {
        let detail = detail.into();
        validate_label("effect_service_detail", &detail, MAX_SERVICE_DETAIL_BYTES)?;
        Ok(Self { kind, detail })
    }

    #[must_use]
    pub const fn kind(&self) -> EffectServiceErrorKind {
        self.kind
    }

    #[must_use]
    pub fn expose_detail(&self) -> &str {
        &self.detail
    }

    pub(crate) fn cancelled() -> Self {
        Self {
            kind: EffectServiceErrorKind::Cancelled,
            detail: "attempt was cancelled before effect execution".to_owned(),
        }
    }

    pub(crate) fn deadline_exceeded() -> Self {
        Self {
            kind: EffectServiceErrorKind::DeadlineExceeded,
            detail: "attempt deadline elapsed before effect execution".to_owned(),
        }
    }
}

impl fmt::Debug for EffectServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectServiceError")
            .field("kind", &self.kind)
            .field("detail", &"[REDACTED]")
            .field("detail_len", &self.detail.len())
            .finish()
    }
}

/// Performs an idempotent effect and returns its durable acknowledgement.
///
/// Implementations must use `request.idempotency_key()` as a durable replay
/// key, call [`EffectBudget::admit`] immediately before the first irreversible
/// commit, and atomically persist the effect result with its receipt where the
/// target supports transactions. Cancellation before admission applies no new
/// effect. Cancellation racing after admission may still commit; in that case
/// the service returns `Applied` (or a reconciled `AlreadyApplied`) rather than
/// reporting cancellation and hiding the committed side effect.
pub trait EffectService: Send + Sync {
    fn execute(
        &self,
        request: &EffectRequest,
        budget: EffectBudget<'_>,
    ) -> Result<EffectReceipt, EffectServiceError>;
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ServiceContractError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} exceeds the {max} byte contract limit")]
    TooLong { field: &'static str, max: usize },
    #[error("{field} exceeds the {max} item contract limit")]
    TooManyItems { field: &'static str, max: usize },
    #[error("{field} contains a NUL byte")]
    Nul { field: &'static str },
    #[error("{field} contains control characters")]
    ControlCharacter { field: &'static str },
    #[error("environment key has invalid syntax")]
    InvalidEnvironmentKey,
    #[error("process exit status is invalid")]
    InvalidExitStatus,
}

/// Compatibility name for the old process-intent input. It now contains the
/// complete process operation and is consumed only by [`ProcessService`].
pub type ProcessIntent = ProcessRequest;
pub type AuthorityIntentError = ServiceContractError;

fn validate_label(
    field: &'static str,
    value: &str,
    max: usize,
) -> Result<(), ServiceContractError> {
    if value.is_empty() {
        return Err(ServiceContractError::Empty { field });
    }
    if value.len() > max {
        return Err(ServiceContractError::TooLong { field, max });
    }
    if value.chars().any(char::is_control) {
        return Err(ServiceContractError::ControlCharacter { field });
    }
    Ok(())
}

fn validate_path(field: &'static str, path: &Path) -> Result<(), ServiceContractError> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.is_empty() {
        return Err(ServiceContractError::Empty { field });
    }
    if bytes.len() > MAX_PATH_BYTES {
        return Err(ServiceContractError::TooLong {
            field,
            max: MAX_PATH_BYTES,
        });
    }
    if bytes.contains(&0) {
        return Err(ServiceContractError::Nul { field });
    }
    Ok(())
}

fn validate_process_value(
    field: &'static str,
    value: &str,
    max: usize,
) -> Result<(), ServiceContractError> {
    if value.len() > max {
        return Err(ServiceContractError::TooLong { field, max });
    }
    if value.as_bytes().contains(&0) {
        return Err(ServiceContractError::Nul { field });
    }
    Ok(())
}

fn validate_environment_key(key: &str) -> Result<(), ServiceContractError> {
    let mut bytes = key.bytes();
    let Some(first) = bytes.next() else {
        return Err(ServiceContractError::Empty {
            field: "environment_key",
        });
    };
    if !first.is_ascii_alphabetic() && first != b'_' {
        return Err(ServiceContractError::InvalidEnvironmentKey);
    }
    if bytes.any(|byte| !byte.is_ascii_alphanumeric() && byte != b'_') {
        return Err(ServiceContractError::InvalidEnvironmentKey);
    }
    Ok(())
}

fn validate_bytes(
    field: &'static str,
    value: &[u8],
    max: usize,
) -> Result<(), ServiceContractError> {
    if value.len() > max {
        return Err(ServiceContractError::TooLong { field, max });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ProcessCapability(u8);

    impl Cancellation for ProcessCapability {
        fn is_cancelled(&self) -> bool {
            self.0 == u8::MAX
        }
    }

    impl ProcessService for ProcessCapability {
        fn finish_preflight(
            &self,
            _request: &ProcessRequest,
            _preflight: ProcessPreflight<'_>,
        ) -> Result<(), ProcessServiceError> {
            unreachable!("capability identity test does not dispatch")
        }

        fn execute(
            &self,
            _request: &ProcessRequest,
            _budget: ProcessBudget,
        ) -> Result<ProcessReceipt, ProcessServiceError> {
            unreachable!("capability identity test does not dispatch")
        }
    }

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    #[test]
    fn identical_request_preflight_cannot_cross_service_capabilities() -> TestResult {
        let first = Box::new(ProcessCapability(1));
        let second = Box::new(ProcessCapability(2));
        let request = ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            "enrolled-provider",
            "/enrolled/work",
        )?;

        let forwarded =
            ProcessPreflight::new(first.as_ref(), &request, ProcessPreflightReason::Cancelled)?;
        assert!(forwarded.bind(second.as_ref(), &request).is_none());

        let local =
            ProcessPreflight::new(first.as_ref(), &request, ProcessPreflightReason::Cancelled)?;
        let bound = local
            .bind(first.as_ref(), &request)
            .ok_or("exact same-service, same-request bind must succeed")?;
        assert!(matches!(bound.reason(), ProcessPreflightReason::Cancelled));
        assert!(
            bound.matches_request(&request),
            "a bound proof must revalidate the exact request it was minted for"
        );
        let other_request = ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            "enrolled-provider",
            "/enrolled/other",
        )?;
        assert!(!bound.matches_request(&other_request));
        Ok(())
    }

    struct ZeroSizedProcessCapability;

    impl Cancellation for ZeroSizedProcessCapability {
        fn is_cancelled(&self) -> bool {
            true
        }
    }

    impl ProcessService for ZeroSizedProcessCapability {
        fn finish_preflight(
            &self,
            _request: &ProcessRequest,
            _preflight: ProcessPreflight<'_>,
        ) -> Result<(), ProcessServiceError> {
            unreachable!("zero-sized capabilities cannot receive a preflight proof")
        }

        fn execute(
            &self,
            _request: &ProcessRequest,
            _budget: ProcessBudget,
        ) -> Result<ProcessReceipt, ProcessServiceError> {
            unreachable!("cancelled zero-sized capability cannot dispatch")
        }
    }

    #[test]
    fn simultaneous_zero_sized_process_capabilities_cannot_cross_bind() -> TestResult {
        let services = [ZeroSizedProcessCapability, ZeroSizedProcessCapability];
        let request = ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            "enrolled-provider",
            "/enrolled/work",
        )?;

        assert!(
            ProcessPreflight::new(&services[0], &request, ProcessPreflightReason::Cancelled)
                .is_err_and(|error| error.kind() == ProcessServiceErrorKind::InvalidRequest),
            "no token may exist to forward from the first live ZST to the second"
        );
        Ok(())
    }
}
