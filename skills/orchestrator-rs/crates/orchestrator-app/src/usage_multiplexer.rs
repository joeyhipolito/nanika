//! Capability-bound usage admission for adaptive-routing dispatch.
//!
//! Trusted composition enrolls reviewed adapters and either produces bound
//! observations for the compatibility assembler or retains them in the
//! in-memory owner. Neither path owns durable floors or provider switching.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
};

use orchestrator_core::{
    ADAPTIVE_ROUTING_SCHEMA_V1, AdaptiveCandidateV1, AdaptiveRoutingError, BasisPointsV1,
    CandidateIdV1, DurationMillisV1, MissionId, PhaseId, ProviderIdV1, RouteRequirementsV1,
    RoutingAuthorityV1, RoutingInputV1, RuntimeIdV1, SnapshotIdV1, UsageAccountIdV1,
    UsageSnapshotV1, UsageSourceIdV1, UsageWindowV1, UtcMillisV1,
};
use orchestrator_provider_claude::{
    CLAUDE_STATUSLINE_SOURCE_ID, ClaudeStatuslineQuotaObservationV1,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Maximum raw observations admitted to one stateless assembly request.
pub const MAX_RAW_USAGE_OBSERVATIONS: usize = 256;
/// Maximum distinct provider/account/source slots admitted to one request.
pub const MAX_USAGE_OBSERVATION_SLOTS: usize = 64;
/// Maximum raw observations admitted for one exact slot.
pub const MAX_RAW_OBSERVATIONS_PER_SLOT: usize = 4;
/// Maximum observations retained in memory across all exact slots.
pub const MAX_RETAINED_USAGE_OBSERVATIONS: usize = MAX_RAW_USAGE_OBSERVATIONS;
/// Maximum exact provider/account/source slots retained in memory.
pub const MAX_RETAINED_USAGE_SLOTS: usize = MAX_USAGE_OBSERVATION_SLOTS;
/// Maximum observations retained in memory for one exact slot.
pub const MAX_RETAINED_OBSERVATIONS_PER_SLOT: usize = MAX_RAW_OBSERVATIONS_PER_SLOT;
/// Maximum caller-owned last-accepted time entries admitted to one request.
pub const MAX_LAST_ACCEPTED_OBSERVATIONS: usize = 256;
/// Maximum dispatch candidates admitted to one request.
pub const MAX_DISPATCH_CANDIDATES: usize = 128;
const MAX_ENROLLED_SOURCES: usize = 64;
const MAX_SCOPE_ENTRIES_PER_SOURCE: usize = 128;
const MAX_TOTAL_SCOPE_ENTRIES: usize = 256;
const TOKEN_BYTES: usize = 32;
const TOKEN_MINT_ATTEMPTS: usize = 8;

const _: () = assert!(
    MAX_RETAINED_USAGE_SLOTS * MAX_RETAINED_OBSERVATIONS_PER_SLOT
        <= MAX_RETAINED_USAGE_OBSERVATIONS
);

/// Stateless admission policy applied after capability-bound normalization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsageMultiplexerPolicy {
    /// Maximum accepted age measured from the provider observation time.
    pub maximum_snapshot_age_ms: DurationMillisV1,
    /// Minimum interval between accepted observations for one account.
    pub cooldown_ms: DurationMillisV1,
}

/// Immutable policy captured when the reviewed Claude adapter is enrolled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClaudeStatuslineAdapterPolicyV1 {
    ttl_ms: DurationMillisV1,
    confidence_bps: BasisPointsV1,
    recent_capacity_bps: BasisPointsV1,
}

impl ClaudeStatuslineAdapterPolicyV1 {
    /// Constructs the immutable normalization policy captured by enrollment.
    ///
    /// `recent_capacity_bps` is local policy because the reviewed Claude
    /// statusline decoder does not provide an authenticated capacity signal.
    pub fn new(
        ttl_ms: DurationMillisV1,
        confidence_bps: BasisPointsV1,
        recent_capacity_bps: BasisPointsV1,
    ) -> Result<Self, TrustedUsageError> {
        if confidence_bps.get() == 0 {
            return Err(TrustedUsageError::ZeroConfidencePolicy);
        }
        Ok(Self {
            ttl_ms,
            confidence_bps,
            recent_capacity_bps,
        })
    }
    /// Maximum lifetime assigned to normalized observations.
    #[must_use]
    pub const fn ttl_ms(self) -> DurationMillisV1 {
        self.ttl_ms
    }
    /// Confidence assigned to normalized observations.
    #[must_use]
    pub const fn confidence_bps(self) -> BasisPointsV1 {
        self.confidence_bps
    }
    /// Recent-capacity value assigned to every decoded provider window.
    #[must_use]
    pub const fn recent_capacity_bps(self) -> BasisPointsV1 {
        self.recent_capacity_bps
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum TrustedUsageError {
    /// A healthy normalized snapshot cannot carry zero confidence.
    #[error("Claude statusline adapter confidence must be non-zero")]
    ZeroConfidencePolicy,
    /// Secure random capability minting did not succeed within its bound.
    #[error("trusted usage capability minting is unavailable")]
    CapabilityMintUnavailable,
    /// A handle was forged or belongs to another owner.
    #[error("trusted usage handle is invalid for this owner")]
    InvalidCapability,
    /// Decoded evidence did not come from the enrolled reviewed adapter.
    #[error("provider usage source does not match the enrolled adapter")]
    SourceMismatch,
    /// The same source/provider/account scope was already enrolled.
    #[error(
        "usage source {source_id:?} is already enrolled for provider {provider:?} account {account:?}"
    )]
    ScopeAlreadyEnrolled {
        /// Stable adapter source identity.
        source_id: String,
        /// Enrolled provider identity.
        provider: String,
        /// Enrolled account identity.
        account: String,
    },
    /// The distinct-source admission limit was reached.
    #[error("trusted usage source capacity is exhausted")]
    EnrolledSourceLimitReached,
    /// One source reached its provider/account scope limit.
    #[error("trusted usage per-source scope capacity is exhausted")]
    SourceScopeLimitReached,
    /// The global provider/account scope limit was reached.
    #[error("trusted usage scope capacity is exhausted")]
    TotalScopeLimitReached,
    /// The trusted observation time followed its receipt time.
    #[error("usage snapshot observation time is later than its receipt time")]
    ObservationAfterReceipt,
    /// A decoded provider reset was outside its declared window.
    #[error("provider usage window timing is invalid for the trusted observation time")]
    InvalidProviderWindowTiming,
    /// An internally derived core identity failed validation.
    #[error("trusted usage adapter produced an invalid normalized identity")]
    InvalidNormalizedIdentity,
    /// An observation did not advance the retained time for its exact slot.
    #[error("usage observation time does not follow the newest retained observation time")]
    NonMonotonicRetainedObservation,
    /// A new exact provider/account/source slot would exceed the retention bound.
    #[error("retained usage slot capacity is exhausted: maximum is {maximum}")]
    RetainedUsageSlotLimitReached {
        /// Maximum exact slots retained by one owner.
        maximum: usize,
    },
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct OpaqueToken([u8; TOKEN_BYTES]);

/// Non-cloneable, non-serializable authority for precisely one enrollment.
pub struct ClaudeStatuslineUsageHandle {
    owner_binding: OpaqueToken,
    enrollment_binding: OpaqueToken,
}
impl fmt::Debug for ClaudeStatuslineUsageHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClaudeStatuslineUsageHandle(<redacted>)")
    }
}

/// Evidence admitted by an enrolled owner.  Its snapshot is intentionally not
/// public, so application callers cannot construct routing evidence directly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundUsageObservation {
    snapshot: UsageSnapshotV1,
}

struct Enrollment {
    source: UsageSourceIdV1,
    provider: ProviderIdV1,
    account: UsageAccountIdV1,
    policy: ClaudeStatuslineAdapterPolicyV1,
}

type UsageSlot = (ProviderIdV1, UsageAccountIdV1, UsageSourceIdV1);

/// Trusted composition owner for bounded Claude statusline enrollments.
pub struct UsageMultiplexer {
    owner_binding: Option<OpaqueToken>,
    sources: BTreeMap<UsageSourceIdV1, BTreeSet<(ProviderIdV1, UsageAccountIdV1)>>,
    total_scopes: usize,
    enrollments: BTreeMap<OpaqueToken, Enrollment>,
    retained: BTreeMap<UsageSlot, VecDeque<BoundUsageObservation>>,
}
impl fmt::Debug for UsageMultiplexer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UsageMultiplexer")
            .field("enrolled_sources", &self.sources.len())
            .field("enrolled_scopes", &self.total_scopes)
            .finish()
    }
}
impl Default for UsageMultiplexer {
    fn default() -> Self {
        Self::new()
    }
}
impl UsageMultiplexer {
    /// Creates an owner with no enrolled adapters or minted owner binding.
    #[must_use]
    pub fn new() -> Self {
        Self {
            owner_binding: None,
            sources: BTreeMap::new(),
            total_scopes: 0,
            enrollments: BTreeMap::new(),
            retained: BTreeMap::new(),
        }
    }

    /// Enrolls the reviewed Claude statusline adapter for one exact identity.
    ///
    /// The source identity is selected from the reviewed provider crate and
    /// the supplied normalization policy is captured immutably. Failed minting
    /// and duplicate or bounded admission leave every enrollment unchanged.
    pub fn enroll_claude_statusline(
        &mut self,
        provider: ProviderIdV1,
        account: UsageAccountIdV1,
        policy: ClaudeStatuslineAdapterPolicyV1,
    ) -> Result<ClaudeStatuslineUsageHandle, TrustedUsageError> {
        let mut entropy = SystemEntropy;
        self.enroll_claude_statusline_with_entropy(provider, account, policy, &mut entropy)
    }

    fn enroll_claude_statusline_with_entropy(
        &mut self,
        provider: ProviderIdV1,
        account: UsageAccountIdV1,
        policy: ClaudeStatuslineAdapterPolicyV1,
        entropy: &mut dyn EntropySource,
    ) -> Result<ClaudeStatuslineUsageHandle, TrustedUsageError> {
        let source = UsageSourceIdV1::new(CLAUDE_STATUSLINE_SOURCE_ID)
            .map_err(|_| TrustedUsageError::InvalidNormalizedIdentity)?;
        self.preflight(&source, &provider, &account)?;
        let owner_binding = match self.owner_binding {
            Some(binding) => binding,
            None => mint_token_with_retries(entropy)?,
        };
        let enrollment_binding = self.mint_enrollment_binding(owner_binding, entropy)?;
        self.sources
            .entry(source.clone())
            .or_default()
            .insert((provider.clone(), account.clone()));
        self.total_scopes += 1;
        self.enrollments.insert(
            enrollment_binding,
            Enrollment {
                source,
                provider,
                account,
                policy,
            },
        );
        self.owner_binding = Some(owner_binding);
        Ok(ClaudeStatuslineUsageHandle {
            owner_binding,
            enrollment_binding,
        })
    }

    /// Binds provider-decoded evidence to an enrolled identity and policy.
    ///
    /// The returned value is the only public observation type accepted by the
    /// dispatch assembler; callers cannot construct or alter its snapshot.
    /// The genuine decoder always emits its reviewed source identity, so the
    /// source-mismatch check is defensive rather than a caller-selectable path.
    pub fn observe_claude_statusline(
        &self,
        handle: &ClaudeStatuslineUsageHandle,
        observation: &ClaudeStatuslineQuotaObservationV1,
        observed_at_utc_ms: UtcMillisV1,
        received_at_utc_ms: UtcMillisV1,
    ) -> Result<BoundUsageObservation, TrustedUsageError> {
        let enrollment = self.resolve(handle)?;
        if observation.source() != &enrollment.source {
            return Err(TrustedUsageError::SourceMismatch);
        }
        Ok(BoundUsageObservation {
            snapshot: normalize(
                handle,
                enrollment,
                observation,
                observed_at_utc_ms,
                received_at_utc_ms,
            )?,
        })
    }

    /// Normalizes and retains one genuine decoded Claude statusline observation.
    ///
    /// The non-cloneable handle selects the exact enrolled provider/account and
    /// its immutable normalization policy; the reviewed decoder supplies the
    /// exact source. Validation, capability resolution, and the per-slot
    /// monotonic-time check all complete before retained state is changed. A
    /// full existing slot drops only its own oldest observation. A new slot at
    /// capacity is rejected without evicting any retained slot.
    pub fn ingest_claude_statusline(
        &mut self,
        handle: &ClaudeStatuslineUsageHandle,
        observation: &ClaudeStatuslineQuotaObservationV1,
        observed_at_utc_ms: UtcMillisV1,
        received_at_utc_ms: UtcMillisV1,
    ) -> Result<(), TrustedUsageError> {
        let bound = self.observe_claude_statusline(
            handle,
            observation,
            observed_at_utc_ms,
            received_at_utc_ms,
        )?;
        let snapshot = &bound.snapshot;
        let slot = (
            snapshot.provider.clone(),
            snapshot.usage_account_id.clone(),
            snapshot.source.clone(),
        );

        if let Some(history) = self.retained.get(&slot) {
            if history
                .back()
                .is_some_and(|newest| newest.snapshot.observed_at_utc_ms >= observed_at_utc_ms)
            {
                return Err(TrustedUsageError::NonMonotonicRetainedObservation);
            }
        } else if self.retained.len() >= MAX_RETAINED_USAGE_SLOTS {
            return Err(TrustedUsageError::RetainedUsageSlotLimitReached {
                maximum: MAX_RETAINED_USAGE_SLOTS,
            });
        }

        let history = self.retained.entry(slot).or_default();
        if history.len() == MAX_RETAINED_OBSERVATIONS_PER_SLOT {
            history.pop_front();
        }
        history.push_back(bound);
        Ok(())
    }

    /// Assembles one routing input from this owner's retained observations.
    ///
    /// The owner and its private snapshots are only borrowed. Exact slots are
    /// traversed in provider/account/source key order and each slot is traversed
    /// oldest first, so output ordering cannot depend on inter-slot arrival
    /// order. Retained history at or below a caller-owned accepted-time floor
    /// has already been accounted for and is omitted; newer history is evaluated
    /// against that floor for cooldown. The floor remains explicit and
    /// non-durable, while the stateless [`assemble_routing_input`] contract is
    /// unchanged.
    pub fn assemble_retained_routing_input(
        &self,
        dispatch: PendingDispatch,
        policy: &UsageMultiplexerPolicy,
        last_accepted: &BTreeMap<(ProviderIdV1, UsageAccountIdV1), UtcMillisV1>,
    ) -> Result<RoutingInputV1, UsageMultiplexerError> {
        if last_accepted.len() > MAX_LAST_ACCEPTED_OBSERVATIONS {
            return Err(UsageMultiplexerError::TooManyLastAcceptedObservations {
                maximum: MAX_LAST_ACCEPTED_OBSERVATIONS,
            });
        }
        if dispatch.candidates.len() > MAX_DISPATCH_CANDIDATES {
            return Err(UsageMultiplexerError::TooManyDispatchCandidates {
                maximum: MAX_DISPATCH_CANDIDATES,
            });
        }
        let observations = self
            .retained
            .values()
            .flat_map(|history| history.iter())
            .filter(|bound| {
                let snapshot = &bound.snapshot;
                last_accepted
                    .get(&(snapshot.provider.clone(), snapshot.usage_account_id.clone()))
                    .is_none_or(|floor| snapshot.observed_at_utc_ms > *floor)
            })
            .collect::<Vec<_>>();
        assemble_routing_input_borrowed(dispatch, policy, &observations, last_accepted)
    }

    fn resolve(
        &self,
        handle: &ClaudeStatuslineUsageHandle,
    ) -> Result<&Enrollment, TrustedUsageError> {
        if self.owner_binding != Some(handle.owner_binding) {
            return Err(TrustedUsageError::InvalidCapability);
        }
        self.enrollments
            .get(&handle.enrollment_binding)
            .ok_or(TrustedUsageError::InvalidCapability)
    }
    fn preflight(
        &self,
        source: &UsageSourceIdV1,
        provider: &ProviderIdV1,
        account: &UsageAccountIdV1,
    ) -> Result<(), TrustedUsageError> {
        if let Some(scopes) = self.sources.get(source) {
            if scopes.contains(&(provider.clone(), account.clone())) {
                return Err(TrustedUsageError::ScopeAlreadyEnrolled {
                    source_id: source.as_str().to_owned(),
                    provider: provider.as_str().to_owned(),
                    account: account.as_str().to_owned(),
                });
            }
            if scopes.len() >= MAX_SCOPE_ENTRIES_PER_SOURCE {
                return Err(TrustedUsageError::SourceScopeLimitReached);
            }
        } else if self.sources.len() >= MAX_ENROLLED_SOURCES {
            return Err(TrustedUsageError::EnrolledSourceLimitReached);
        }
        if self.total_scopes >= MAX_TOTAL_SCOPE_ENTRIES {
            return Err(TrustedUsageError::TotalScopeLimitReached);
        }
        Ok(())
    }
    fn mint_enrollment_binding(
        &self,
        owner: OpaqueToken,
        entropy: &mut dyn EntropySource,
    ) -> Result<OpaqueToken, TrustedUsageError> {
        for _ in 0..TOKEN_MINT_ATTEMPTS {
            if let Ok(token) = try_mint_token(entropy) {
                if token != owner && !self.enrollments.contains_key(&token) {
                    return Ok(token);
                }
            }
        }
        Err(TrustedUsageError::CapabilityMintUnavailable)
    }
}

trait EntropySource {
    fn fill(&mut self, bytes: &mut [u8; TOKEN_BYTES]) -> Result<(), ()>;
}

struct SystemEntropy;

impl EntropySource for SystemEntropy {
    fn fill(&mut self, bytes: &mut [u8; TOKEN_BYTES]) -> Result<(), ()> {
        getrandom::fill(bytes).map_err(|_| ())
    }
}

fn try_mint_token(entropy: &mut dyn EntropySource) -> Result<OpaqueToken, ()> {
    let mut bytes = [0; TOKEN_BYTES];
    entropy.fill(&mut bytes)?;
    Ok(OpaqueToken(bytes))
}

fn mint_token_with_retries(
    entropy: &mut dyn EntropySource,
) -> Result<OpaqueToken, TrustedUsageError> {
    for _ in 0..TOKEN_MINT_ATTEMPTS {
        if let Ok(token) = try_mint_token(entropy) {
            return Ok(token);
        }
    }
    Err(TrustedUsageError::CapabilityMintUnavailable)
}
fn snapshot_id(
    handle: &ClaudeStatuslineUsageHandle,
    observed: UtcMillisV1,
    received: UtcMillisV1,
) -> Result<SnapshotIdV1, TrustedUsageError> {
    let mut hash = Sha256::new();
    hash.update(b"orchestrator-app/trusted-usage-snapshot/v1\0");
    hash.update(handle.owner_binding.0);
    hash.update(handle.enrollment_binding.0);
    hash.update(observed.get().to_be_bytes());
    hash.update(received.get().to_be_bytes());
    let digest = hash.finalize();
    SnapshotIdV1::new(format!("usage-{:x}", digest))
        .map_err(|_| TrustedUsageError::InvalidNormalizedIdentity)
}
fn normalize(
    handle: &ClaudeStatuslineUsageHandle,
    enrollment: &Enrollment,
    observation: &ClaudeStatuslineQuotaObservationV1,
    observed: UtcMillisV1,
    received: UtcMillisV1,
) -> Result<UsageSnapshotV1, TrustedUsageError> {
    if observed > received {
        return Err(TrustedUsageError::ObservationAfterReceipt);
    }
    let mut windows = Vec::with_capacity(observation.windows().len());
    for window in observation.windows() {
        if window.resets_at_utc_ms() <= observed
            || window.resets_at_utc_ms().get() - observed.get() > window.duration_ms().get()
        {
            return Err(TrustedUsageError::InvalidProviderWindowTiming);
        }
        windows.push(UsageWindowV1 {
            limit_id: window.limit_id().clone(),
            kind: window.kind().clone(),
            applicable_models: BTreeSet::new(),
            used_bps: window.used_bps(),
            remaining_bps: window.remaining_bps(),
            recent_capacity_bps: enrollment.policy.recent_capacity_bps(),
            resets_at_utc_ms: window.resets_at_utc_ms(),
            duration_ms: window.duration_ms(),
        });
    }
    Ok(UsageSnapshotV1 {
        schema_version: ADAPTIVE_ROUTING_SCHEMA_V1,
        snapshot_id: snapshot_id(handle, observed, received)?,
        provider: enrollment.provider.clone(),
        usage_account_id: enrollment.account.clone(),
        source: enrollment.source.clone(),
        observed_at_utc_ms: observed,
        received_at_utc_ms: received,
        ttl_ms: enrollment.policy.ttl_ms(),
        confidence_bps: enrollment.policy.confidence_bps(),
        health: observation.health(),
        reason_code: observation.reason_code().cloned(),
        windows,
    })
}

/// Result of evaluating one capability-bound observation for one dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UsageObservationOutcome {
    /// The normalized snapshot is eligible routing evidence.
    Bound(UsageSnapshotV1),
    /// The observation reached its effective adapter or multiplexer TTL.
    Expired,
    /// The observation advanced time but arrived inside the cooldown.
    CoolingDown,
    /// The decoded provider observation contained no usage windows.
    Unusable,
}
/// Fail-closed errors from stateless observation assembly.
#[derive(Debug, Error)]
pub enum UsageMultiplexerError {
    /// A normalized value violated the adaptive-routing core contract.
    #[error(transparent)]
    Core(#[from] AdaptiveRoutingError),
    /// Dispatch evaluation preceded local receipt of an observation.
    #[error("evaluation time precedes an observation's local receipt time")]
    EvaluationBeforeReceipt,
    /// An observation claimed a time after local receipt.
    #[error("observation time follows its local receipt time")]
    ObservationAfterReceipt,
    /// An observation did not advance its account's accepted-time floor.
    #[error("observation time does not follow the last accepted observation time")]
    NonMonotonicObservation,
    /// A provider reset was outside its declared window duration.
    #[error("provider window reset timing is inconsistent with its declared duration")]
    InvalidProviderWindowTiming,
    /// The total raw-observation limit was exceeded.
    #[error("too many raw usage observations: maximum is {maximum}")]
    TooManyRawObservations {
        /// Maximum raw observations admitted to one request.
        maximum: usize,
    },
    /// The distinct-slot limit was exceeded.
    #[error("too many usage observation slots: maximum is {maximum}")]
    TooManyUsageObservationSlots {
        /// Maximum provider/account/source slots admitted to one request.
        maximum: usize,
    },
    /// One slot exceeded its raw-observation limit.
    #[error("too many raw usage observations for one slot: maximum is {maximum}")]
    TooManyRawObservationsPerSlot {
        /// Maximum raw observations admitted for one exact slot.
        maximum: usize,
    },
    /// The caller-owned accepted-time floor map exceeded its limit.
    #[error("too many last-accepted observation entries: maximum is {maximum}")]
    TooManyLastAcceptedObservations {
        /// Maximum accepted-time floor entries admitted to one request.
        maximum: usize,
    },
    /// The dispatch candidate catalog exceeded its limit.
    #[error("too many dispatch candidates: maximum is {maximum}")]
    TooManyDispatchCandidates {
        /// Maximum candidates admitted to one request.
        maximum: usize,
    },
}

fn evaluate_bound_observation(
    policy: &UsageMultiplexerPolicy,
    evaluated: UtcMillisV1,
    last: Option<UtcMillisV1>,
    bound: &BoundUsageObservation,
) -> Result<UsageObservationOutcome, UsageMultiplexerError> {
    let snapshot = &bound.snapshot;
    if snapshot.observed_at_utc_ms > snapshot.received_at_utc_ms {
        return Err(UsageMultiplexerError::ObservationAfterReceipt);
    }
    if evaluated < snapshot.received_at_utc_ms {
        return Err(UsageMultiplexerError::EvaluationBeforeReceipt);
    }
    for window in &snapshot.windows {
        if window.resets_at_utc_ms <= snapshot.observed_at_utc_ms
            || window.resets_at_utc_ms.get() - snapshot.observed_at_utc_ms.get()
                > window.duration_ms.get()
        {
            return Err(UsageMultiplexerError::InvalidProviderWindowTiming);
        }
    }
    if let Some(last) = last {
        if snapshot.observed_at_utc_ms <= last {
            return Err(UsageMultiplexerError::NonMonotonicObservation);
        }
        if snapshot.observed_at_utc_ms.get() - last.get() < policy.cooldown_ms.get() {
            return Ok(UsageObservationOutcome::CoolingDown);
        }
    }
    if evaluated.get() - snapshot.observed_at_utc_ms.get()
        >= snapshot
            .ttl_ms
            .get()
            .min(policy.maximum_snapshot_age_ms.get())
    {
        return Ok(UsageObservationOutcome::Expired);
    }
    if snapshot.windows.is_empty() {
        return Ok(UsageObservationOutcome::Unusable);
    }
    Ok(UsageObservationOutcome::Bound(snapshot.clone()))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingDispatch {
    /// Mission identity used for audit correlation.
    pub mission_id: MissionId,
    /// Phase identity used for audit correlation.
    pub phase_id: PhaseId,
    /// One-based dispatch attempt number.
    pub attempt: u32,
    /// Caller-captured evaluation time.
    pub evaluated_at_utc_ms: UtcMillisV1,
    /// Fixed authority and legacy route resolved before adaptive policy.
    pub authority: RoutingAuthorityV1,
    /// Hard requirements every candidate must satisfy.
    pub requirements: RouteRequirementsV1,
    /// Adaptive candidate catalog.
    pub candidates: Vec<AdaptiveCandidateV1>,
    /// Current candidate identity used for hysteresis.
    pub incumbent_candidate_id: Option<CandidateIdV1>,
    /// Existing session runtime family used for continuation decisions.
    pub existing_session_runtime: Option<RuntimeIdV1>,
}

/// Statelessly assembles routing input from privately constructed evidence.
///
/// Structural limits are checked before observation evaluation. Excluded
/// expired, cooling-down, and unusable observations preserve input order among
/// the snapshots that remain; typed timing errors fail the whole assembly.
pub fn assemble_routing_input(
    dispatch: PendingDispatch,
    policy: &UsageMultiplexerPolicy,
    observations: &[BoundUsageObservation],
    last_accepted: &BTreeMap<(ProviderIdV1, UsageAccountIdV1), UtcMillisV1>,
) -> Result<RoutingInputV1, UsageMultiplexerError> {
    if observations.len() > MAX_RAW_USAGE_OBSERVATIONS {
        return Err(UsageMultiplexerError::TooManyRawObservations {
            maximum: MAX_RAW_USAGE_OBSERVATIONS,
        });
    }
    let observations = observations.iter().collect::<Vec<_>>();
    assemble_routing_input_borrowed(dispatch, policy, &observations, last_accepted)
}

fn assemble_routing_input_borrowed(
    dispatch: PendingDispatch,
    policy: &UsageMultiplexerPolicy,
    observations: &[&BoundUsageObservation],
    last_accepted: &BTreeMap<(ProviderIdV1, UsageAccountIdV1), UtcMillisV1>,
) -> Result<RoutingInputV1, UsageMultiplexerError> {
    if observations.len() > MAX_RAW_USAGE_OBSERVATIONS {
        return Err(UsageMultiplexerError::TooManyRawObservations {
            maximum: MAX_RAW_USAGE_OBSERVATIONS,
        });
    }
    if last_accepted.len() > MAX_LAST_ACCEPTED_OBSERVATIONS {
        return Err(UsageMultiplexerError::TooManyLastAcceptedObservations {
            maximum: MAX_LAST_ACCEPTED_OBSERVATIONS,
        });
    }
    if dispatch.candidates.len() > MAX_DISPATCH_CANDIDATES {
        return Err(UsageMultiplexerError::TooManyDispatchCandidates {
            maximum: MAX_DISPATCH_CANDIDATES,
        });
    }
    let mut slots = BTreeMap::new();
    for bound in observations {
        let snapshot = &bound.snapshot;
        let key = (
            &snapshot.provider,
            &snapshot.usage_account_id,
            &snapshot.source,
        );
        if let Some(count) = slots.get_mut(&key) {
            if *count >= MAX_RAW_OBSERVATIONS_PER_SLOT {
                return Err(UsageMultiplexerError::TooManyRawObservationsPerSlot {
                    maximum: MAX_RAW_OBSERVATIONS_PER_SLOT,
                });
            }
            *count += 1;
        } else {
            if slots.len() >= MAX_USAGE_OBSERVATION_SLOTS {
                return Err(UsageMultiplexerError::TooManyUsageObservationSlots {
                    maximum: MAX_USAGE_OBSERVATION_SLOTS,
                });
            }
            slots.insert(key, 1_usize);
        }
    }
    let mut usage_snapshots = Vec::with_capacity(observations.len());
    for bound in observations {
        let snapshot = &bound.snapshot;
        let last = last_accepted
            .get(&(snapshot.provider.clone(), snapshot.usage_account_id.clone()))
            .copied();
        if let UsageObservationOutcome::Bound(snapshot) =
            evaluate_bound_observation(policy, dispatch.evaluated_at_utc_ms, last, bound)?
        {
            usage_snapshots.push(snapshot);
        }
    }
    Ok(RoutingInputV1 {
        schema_version: ADAPTIVE_ROUTING_SCHEMA_V1,
        mission_id: dispatch.mission_id,
        phase_id: dispatch.phase_id,
        attempt: dispatch.attempt,
        evaluated_at_utc_ms: dispatch.evaluated_at_utc_ms,
        authority: dispatch.authority,
        requirements: dispatch.requirements,
        candidates: dispatch.candidates,
        usage_snapshots,
        incumbent_candidate_id: dispatch.incumbent_candidate_id,
        existing_session_runtime: dispatch.existing_session_runtime,
    })
}

// Durable observed-time floors/restart persistence and live quota switching
// intentionally remain outside these in-memory composition boundaries.

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    use orchestrator_core::{
        AdaptiveEvaluationModeV1, AdaptivePolicyV1, AutomaticEligibilityV1, ProviderReserveV1,
        QualityTierV1, RouteTargetV1, ScoreWeightsV1, TaskPriorityV1, UnknownUsagePolicyV1,
        decide_route,
    };
    use orchestrator_provider_claude::decode_claude_statusline_usage;

    type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    const RESET_AT_SECONDS: u64 = 1_738_425_600;
    const RESET_AT_MS: u64 = RESET_AT_SECONDS * 1_000;
    const OBSERVED_AT_MS: u64 = RESET_AT_MS - 60_000;
    const FIVE_HOURS_MS: u64 = 5 * 60 * 60 * 1_000;

    fn adapter_policy() -> Result<ClaudeStatuslineAdapterPolicyV1> {
        Ok(ClaudeStatuslineAdapterPolicyV1::new(
            DurationMillisV1::new(120_000)?,
            BasisPointsV1::new(10_000)?,
            BasisPointsV1::new(9_000)?,
        )?)
    }

    fn multiplexer_policy() -> Result<UsageMultiplexerPolicy> {
        multiplexer_policy_with_cooldown(60_000)
    }

    fn multiplexer_policy_with_cooldown(cooldown_ms: u64) -> Result<UsageMultiplexerPolicy> {
        Ok(UsageMultiplexerPolicy {
            maximum_snapshot_age_ms: DurationMillisV1::new(300_000)?,
            cooldown_ms: DurationMillisV1::new(cooldown_ms)?,
        })
    }

    fn healthy_observation() -> Result<ClaudeStatuslineQuotaObservationV1> {
        Ok(decode_claude_statusline_usage(
            br#"{
              "version":"2.1.211",
              "rate_limits":{
                "five_hour":{"used_percentage":10,"resets_at":1738425600},
                "seven_day":{"used_percentage":20,"resets_at":1738857600}
              }
            }"#,
        )?)
    }

    fn empty_observation() -> Result<ClaudeStatuslineQuotaObservationV1> {
        Ok(decode_claude_statusline_usage(br#"{"version":"2.1.211"}"#)?)
    }

    fn five_hour_observation() -> Result<ClaudeStatuslineQuotaObservationV1> {
        let bytes = serde_json::json!({
            "version": "2.1.211",
            "rate_limits": {
                "five_hour": {
                    "used_percentage": 10,
                    "resets_at": RESET_AT_SECONDS,
                },
            },
        })
        .to_string();
        Ok(decode_claude_statusline_usage(bytes.as_bytes())?)
    }

    fn enroll(owner: &mut UsageMultiplexer, account: &str) -> Result<ClaudeStatuslineUsageHandle> {
        Ok(owner.enroll_claude_statusline(
            ProviderIdV1::new("claude")?,
            UsageAccountIdV1::new(account)?,
            adapter_policy()?,
        )?)
    }

    fn bind(
        account: &str,
        observed_at_ms: u64,
        received_at_ms: u64,
        observation: &ClaudeStatuslineQuotaObservationV1,
    ) -> Result<BoundUsageObservation> {
        let mut owner = UsageMultiplexer::new();
        let handle = enroll(&mut owner, account)?;
        Ok(owner.observe_claude_statusline(
            &handle,
            observation,
            UtcMillisV1::new(observed_at_ms),
            UtcMillisV1::new(received_at_ms),
        )?)
    }

    fn bound_observations(
        slot_count: usize,
        observations_per_slot: usize,
    ) -> Result<Vec<BoundUsageObservation>> {
        let mut owner = UsageMultiplexer::new();
        let mut handles = Vec::with_capacity(slot_count);
        for slot in 0..slot_count {
            handles.push(enroll(&mut owner, &format!("acct-slot-{slot}"))?);
        }
        let observation = healthy_observation()?;
        let mut bound = Vec::with_capacity(slot_count * observations_per_slot);
        for handle in &handles {
            for offset in 0..observations_per_slot {
                bound.push(owner.observe_claude_statusline(
                    handle,
                    &observation,
                    UtcMillisV1::new(OBSERVED_AT_MS + offset as u64),
                    UtcMillisV1::new(OBSERVED_AT_MS + offset as u64),
                )?);
            }
        }
        Ok(bound)
    }

    fn fixed_candidate() -> Result<AdaptiveCandidateV1> {
        Ok(AdaptiveCandidateV1 {
            route: RouteTargetV1 {
                candidate_id: CandidateIdV1::new("claude-primary")?,
                provider: ProviderIdV1::new("claude")?,
                usage_account_id: UsageAccountIdV1::new("acct-primary")?,
                runtime: RuntimeIdV1::new("claude")?,
                model: "claude-sonnet-5".to_owned(),
                effort: None,
            },
            capabilities: BTreeSet::new(),
            automatic_eligibility: AutomaticEligibilityV1::Automatic,
            quality: QualityTierV1::Standard,
            task_fit_bps: BasisPointsV1::new(10_000)?,
            latency_bps: BasisPointsV1::new(10_000)?,
            configured_preference_bps: BasisPointsV1::new(10_000)?,
        })
    }

    fn fixed_policy() -> Result<AdaptivePolicyV1> {
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
            evaluation_mode: AdaptiveEvaluationModeV1::Enforce,
        })
    }

    fn pending_dispatch(
        evaluated_at_ms: u64,
        candidates: Vec<AdaptiveCandidateV1>,
    ) -> Result<PendingDispatch> {
        Ok(PendingDispatch {
            mission_id: MissionId::new("20260826-usage-mux-bounds")?,
            phase_id: PhaseId::new("dispatch")?,
            attempt: 1,
            evaluated_at_utc_ms: UtcMillisV1::new(evaluated_at_ms),
            authority: RoutingAuthorityV1 {
                resolved_fixed_route: None,
                legacy_route: RouteTargetV1 {
                    candidate_id: CandidateIdV1::new("claude-primary")?,
                    provider: ProviderIdV1::new("claude")?,
                    usage_account_id: UsageAccountIdV1::new("acct-primary")?,
                    runtime: RuntimeIdV1::new("claude")?,
                    model: "claude-sonnet-5".to_owned(),
                    effort: None,
                },
            },
            requirements: RouteRequirementsV1 {
                priority: TaskPriorityV1::P1,
                required_capabilities: BTreeSet::new(),
                minimum_quality: QualityTierV1::Economy,
            },
            candidates,
            incumbent_candidate_id: None,
            existing_session_runtime: None,
        })
    }

    fn assemble(
        observations: &[BoundUsageObservation],
        evaluated_at_ms: u64,
        policy: &UsageMultiplexerPolicy,
        last_accepted: &BTreeMap<(ProviderIdV1, UsageAccountIdV1), UtcMillisV1>,
    ) -> std::result::Result<RoutingInputV1, UsageMultiplexerError> {
        assemble_routing_input(
            pending_dispatch(
                evaluated_at_ms,
                vec![fixed_candidate().expect("valid candidate")],
            )
            .expect("valid dispatch"),
            policy,
            observations,
            last_accepted,
        )
    }

    #[derive(Clone, Copy)]
    enum EntropyStep {
        Bytes(u8),
        Failure,
    }

    struct ScriptedEntropy {
        steps: VecDeque<EntropyStep>,
        calls: usize,
    }

    impl ScriptedEntropy {
        fn new(steps: impl IntoIterator<Item = EntropyStep>) -> Self {
            Self {
                steps: steps.into_iter().collect(),
                calls: 0,
            }
        }
    }

    impl EntropySource for ScriptedEntropy {
        fn fill(&mut self, bytes: &mut [u8; TOKEN_BYTES]) -> std::result::Result<(), ()> {
            self.calls += 1;
            match self.steps.pop_front().ok_or(())? {
                EntropyStep::Bytes(value) => {
                    bytes.fill(value);
                    Ok(())
                }
                EntropyStep::Failure => Err(()),
            }
        }
    }

    #[test]
    fn policy_rejects_zero_confidence() -> Result {
        assert_eq!(
            ClaudeStatuslineAdapterPolicyV1::new(
                DurationMillisV1::new(60_000)?,
                BasisPointsV1::new(0)?,
                BasisPointsV1::new(5_000)?,
            ),
            Err(TrustedUsageError::ZeroConfidencePolicy)
        );
        Ok(())
    }

    #[test]
    fn enrollment_immutably_binds_identity_policy_and_capacity_through_public_assembly() -> Result {
        let mut owner = UsageMultiplexer::new();
        let captured_policy = ClaudeStatuslineAdapterPolicyV1::new(
            DurationMillisV1::new(120_000)?,
            BasisPointsV1::new(9_250)?,
            BasisPointsV1::new(6_750)?,
        )?;
        let handle = owner.enroll_claude_statusline(
            ProviderIdV1::new("anthropic")?,
            UsageAccountIdV1::new("primary")?,
            captured_policy,
        )?;
        let bound = owner.observe_claude_statusline(
            &handle,
            &healthy_observation()?,
            UtcMillisV1::new(OBSERVED_AT_MS),
            UtcMillisV1::new(OBSERVED_AT_MS + 100),
        )?;
        let input = assemble(
            &[bound],
            OBSERVED_AT_MS + 1_000,
            &multiplexer_policy()?,
            &BTreeMap::new(),
        )?;
        let snapshot = input.usage_snapshots.first().ok_or("missing snapshot")?;
        assert_eq!(snapshot.provider.as_str(), "anthropic");
        assert_eq!(snapshot.usage_account_id.as_str(), "primary");
        assert_eq!(snapshot.source.as_str(), CLAUDE_STATUSLINE_SOURCE_ID);
        assert_eq!(snapshot.ttl_ms, captured_policy.ttl_ms());
        assert_eq!(snapshot.confidence_bps, captured_policy.confidence_bps());
        assert!(
            snapshot
                .windows
                .iter()
                .all(|window| window.recent_capacity_bps == captured_policy.recent_capacity_bps())
        );
        Ok(())
    }

    #[test]
    fn snapshot_ids_are_stable_per_admission_and_owner_distinct_through_public_assembly() -> Result
    {
        let observed = UtcMillisV1::new(OBSERVED_AT_MS);
        let received = UtcMillisV1::new(OBSERVED_AT_MS + 1);
        let decoded = healthy_observation()?;
        let mut first = UsageMultiplexer::new();
        let first_handle = enroll(&mut first, "acct-primary")?;
        let mut second = UsageMultiplexer::new();
        let second_handle = enroll(&mut second, "acct-primary")?;
        let first_bound =
            first.observe_claude_statusline(&first_handle, &decoded, observed, received)?;
        let first_again =
            first.observe_claude_statusline(&first_handle, &decoded, observed, received)?;
        let second_bound =
            second.observe_claude_statusline(&second_handle, &decoded, observed, received)?;

        let policy = multiplexer_policy()?;
        let first_input = assemble(
            &[first_bound],
            OBSERVED_AT_MS + 1_000,
            &policy,
            &BTreeMap::new(),
        )?;
        let repeated_input = assemble(
            &[first_again],
            OBSERVED_AT_MS + 1_000,
            &policy,
            &BTreeMap::new(),
        )?;
        let second_input = assemble(
            &[second_bound],
            OBSERVED_AT_MS + 1_000,
            &policy,
            &BTreeMap::new(),
        )?;
        assert_eq!(
            first_input.usage_snapshots[0].snapshot_id,
            repeated_input.usage_snapshots[0].snapshot_id
        );
        assert_ne!(
            first_input.usage_snapshots[0].snapshot_id,
            second_input.usage_snapshots[0].snapshot_id
        );
        Ok(())
    }

    #[test]
    fn forged_and_cross_owner_handles_are_rejected_without_owner_mutation() -> Result {
        let mut origin = UsageMultiplexer::new();
        let handle = enroll(&mut origin, "acct-primary")?;
        let mut other = UsageMultiplexer::new();
        let other_handle = enroll(&mut other, "acct-primary")?;
        let before = format!("{other:?}");
        let decoded = healthy_observation()?;

        assert_eq!(
            other.observe_claude_statusline(
                &handle,
                &decoded,
                UtcMillisV1::new(OBSERVED_AT_MS),
                UtcMillisV1::new(OBSERVED_AT_MS),
            ),
            Err(TrustedUsageError::InvalidCapability)
        );
        let mut forged_bytes = other_handle.enrollment_binding.0;
        forged_bytes[0] ^= 0xff;
        let forged = ClaudeStatuslineUsageHandle {
            owner_binding: other_handle.owner_binding,
            enrollment_binding: OpaqueToken(forged_bytes),
        };
        assert_eq!(
            other.observe_claude_statusline(
                &forged,
                &decoded,
                UtcMillisV1::new(OBSERVED_AT_MS),
                UtcMillisV1::new(OBSERVED_AT_MS),
            ),
            Err(TrustedUsageError::InvalidCapability)
        );
        assert_eq!(format!("{other:?}"), before);

        let valid = other.observe_claude_statusline(
            &other_handle,
            &decoded,
            UtcMillisV1::new(OBSERVED_AT_MS),
            UtcMillisV1::new(OBSERVED_AT_MS),
        )?;
        let input = assemble(
            &[valid],
            OBSERVED_AT_MS + 1,
            &multiplexer_policy()?,
            &BTreeMap::new(),
        )?;
        assert_eq!(input.usage_snapshots.len(), 1);
        Ok(())
    }

    #[test]
    fn fixed_decoder_source_mismatch_is_rejected_at_private_negative_seam() -> Result {
        let mut owner = UsageMultiplexer::new();
        let handle = enroll(&mut owner, "acct-primary")?;
        let before = format!("{owner:?}");
        // The genuine decoder always constructs the reviewed source, so a
        // mismatch is only reachable by corrupting private enrollment state.
        owner
            .enrollments
            .get_mut(&handle.enrollment_binding)
            .ok_or("missing enrollment")?
            .source = UsageSourceIdV1::new("different_reviewed_adapter")?;
        assert_eq!(
            owner.observe_claude_statusline(
                &handle,
                &healthy_observation()?,
                UtcMillisV1::new(OBSERVED_AT_MS),
                UtcMillisV1::new(OBSERVED_AT_MS),
            ),
            Err(TrustedUsageError::SourceMismatch)
        );
        assert_eq!(format!("{owner:?}"), before);
        Ok(())
    }

    #[test]
    fn duplicate_and_bounded_enrollment_preserve_the_original_scope() -> Result {
        let mut owner = UsageMultiplexer::new();
        let first = enroll(&mut owner, "account-000")?;
        let replacement_policy = ClaudeStatuslineAdapterPolicyV1::new(
            DurationMillisV1::new(1)?,
            BasisPointsV1::new(1)?,
            BasisPointsV1::new(1)?,
        )?;
        assert!(matches!(
            owner.enroll_claude_statusline(
                ProviderIdV1::new("claude")?,
                UsageAccountIdV1::new("account-000")?,
                replacement_policy,
            ),
            Err(TrustedUsageError::ScopeAlreadyEnrolled { .. })
        ));
        for index in 1..MAX_SCOPE_ENTRIES_PER_SOURCE {
            let _handle = enroll(&mut owner, &format!("account-{index:03}"))?;
        }
        assert_eq!(owner.total_scopes, MAX_SCOPE_ENTRIES_PER_SOURCE);
        assert!(matches!(
            owner.enroll_claude_statusline(
                ProviderIdV1::new("claude")?,
                UsageAccountIdV1::new("overflow")?,
                adapter_policy()?,
            ),
            Err(TrustedUsageError::SourceScopeLimitReached)
        ));

        let bound = owner.observe_claude_statusline(
            &first,
            &healthy_observation()?,
            UtcMillisV1::new(OBSERVED_AT_MS),
            UtcMillisV1::new(OBSERVED_AT_MS),
        )?;
        let input = assemble(
            &[bound],
            OBSERVED_AT_MS + 1,
            &multiplexer_policy()?,
            &BTreeMap::new(),
        )?;
        assert_eq!(input.usage_snapshots[0].ttl_ms, adapter_policy()?.ttl_ms());
        Ok(())
    }

    #[test]
    fn handle_and_owner_debug_redact_known_private_bindings() -> Result {
        let mut entropy =
            ScriptedEntropy::new([EntropyStep::Bytes(0xa5), EntropyStep::Bytes(0x5a)]);
        let mut owner = UsageMultiplexer::new();
        let handle = owner.enroll_claude_statusline_with_entropy(
            ProviderIdV1::new("claude")?,
            UsageAccountIdV1::new("acct-primary")?,
            adapter_policy()?,
            &mut entropy,
        )?;
        assert_eq!(
            format!("{handle:?}"),
            "ClaudeStatuslineUsageHandle(<redacted>)"
        );
        assert_eq!(
            format!("{owner:?}"),
            "UsageMultiplexer { enrolled_sources: 1, enrolled_scopes: 1 }"
        );
        assert!(!format!("{handle:?}{owner:?}").contains("a5a5"));
        assert!(!format!("{handle:?}{owner:?}").contains("5a5a"));
        Ok(())
    }

    #[test]
    fn entropy_retries_failures_and_collisions_and_mints_owner_only_once() -> Result {
        let mut entropy = ScriptedEntropy::new([
            EntropyStep::Failure,
            EntropyStep::Bytes(1),
            EntropyStep::Failure,
            EntropyStep::Bytes(1),
            EntropyStep::Bytes(2),
            EntropyStep::Bytes(2),
            EntropyStep::Failure,
            EntropyStep::Bytes(3),
        ]);
        let mut owner = UsageMultiplexer::new();
        let _first = owner.enroll_claude_statusline_with_entropy(
            ProviderIdV1::new("claude")?,
            UsageAccountIdV1::new("first")?,
            adapter_policy()?,
            &mut entropy,
        )?;
        assert_eq!(entropy.calls, 5);
        let _second = owner.enroll_claude_statusline_with_entropy(
            ProviderIdV1::new("claude")?,
            UsageAccountIdV1::new("second")?,
            adapter_policy()?,
            &mut entropy,
        )?;
        assert_eq!(entropy.calls, 8);
        assert!(owner.owner_binding == Some(OpaqueToken([1; TOKEN_BYTES])));
        assert_eq!(owner.total_scopes, 2);
        Ok(())
    }

    #[test]
    fn entropy_exhaustion_is_bounded_and_transactional() -> Result {
        let mut unavailable = ScriptedEntropy::new([EntropyStep::Failure; TOKEN_MINT_ATTEMPTS]);
        let mut empty_owner = UsageMultiplexer::new();
        assert!(matches!(
            empty_owner.enroll_claude_statusline_with_entropy(
                ProviderIdV1::new("claude")?,
                UsageAccountIdV1::new("never-enrolled")?,
                adapter_policy()?,
                &mut unavailable,
            ),
            Err(TrustedUsageError::CapabilityMintUnavailable)
        ));
        assert_eq!(unavailable.calls, TOKEN_MINT_ATTEMPTS);
        assert!(empty_owner.owner_binding.is_none());
        assert!(empty_owner.sources.is_empty());
        assert!(empty_owner.enrollments.is_empty());
        assert_eq!(empty_owner.total_scopes, 0);

        let mut collisions = ScriptedEntropy::new(
            std::iter::once(EntropyStep::Bytes(7))
                .chain([EntropyStep::Bytes(7); TOKEN_MINT_ATTEMPTS]),
        );
        let mut collision_owner = UsageMultiplexer::new();
        assert!(matches!(
            collision_owner.enroll_claude_statusline_with_entropy(
                ProviderIdV1::new("claude")?,
                UsageAccountIdV1::new("collision")?,
                adapter_policy()?,
                &mut collisions,
            ),
            Err(TrustedUsageError::CapabilityMintUnavailable)
        ));
        assert_eq!(collisions.calls, TOKEN_MINT_ATTEMPTS + 1);
        assert!(collision_owner.owner_binding.is_none());
        assert!(collision_owner.sources.is_empty());
        assert!(collision_owner.enrollments.is_empty());
        assert_eq!(collision_owner.total_scopes, 0);

        let mut initial_entropy =
            ScriptedEntropy::new([EntropyStep::Bytes(1), EntropyStep::Bytes(2)]);
        let mut established_owner = UsageMultiplexer::new();
        let existing = established_owner.enroll_claude_statusline_with_entropy(
            ProviderIdV1::new("claude")?,
            UsageAccountIdV1::new("existing")?,
            adapter_policy()?,
            &mut initial_entropy,
        )?;
        let established_binding = established_owner.owner_binding;
        let mut exhausted = ScriptedEntropy::new([EntropyStep::Failure; TOKEN_MINT_ATTEMPTS]);
        assert!(matches!(
            established_owner.enroll_claude_statusline_with_entropy(
                ProviderIdV1::new("claude")?,
                UsageAccountIdV1::new("not-added")?,
                adapter_policy()?,
                &mut exhausted,
            ),
            Err(TrustedUsageError::CapabilityMintUnavailable)
        ));
        assert_eq!(exhausted.calls, TOKEN_MINT_ATTEMPTS);
        assert!(established_owner.owner_binding == established_binding);
        assert_eq!(established_owner.sources.len(), 1);
        assert_eq!(established_owner.total_scopes, 1);
        assert_eq!(established_owner.enrollments.len(), 1);
        let still_valid = established_owner.observe_claude_statusline(
            &existing,
            &healthy_observation()?,
            UtcMillisV1::new(OBSERVED_AT_MS),
            UtcMillisV1::new(OBSERVED_AT_MS),
        )?;
        assert_eq!(
            assemble(
                &[still_valid],
                OBSERVED_AT_MS + 1,
                &multiplexer_policy()?,
                &BTreeMap::new(),
            )?
            .usage_snapshots
            .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn ttl_expiry_is_measured_from_observation_time_not_receipt() -> Result {
        let policy = multiplexer_policy()?;
        let decoded = healthy_observation()?;
        let stale = bind("acct-primary", RESET_AT_MS - 300_000, RESET_AT_MS, &decoded)?;
        let stale_input = assemble(&[stale], RESET_AT_MS, &policy, &BTreeMap::new())?;
        assert!(stale_input.usage_snapshots.is_empty());

        let fresh = bind(
            "acct-primary",
            RESET_AT_MS - 119_999,
            RESET_AT_MS - 119_999,
            &decoded,
        )?;
        let fresh_input = assemble(&[fresh], RESET_AT_MS, &policy, &BTreeMap::new())?;
        assert_eq!(fresh_input.usage_snapshots.len(), 1);
        Ok(())
    }

    #[test]
    fn cooldown_suppresses_too_soon_and_accepts_the_exact_boundary() -> Result {
        let policy = multiplexer_policy()?;
        let bound = bind(
            "acct-primary",
            OBSERVED_AT_MS,
            OBSERVED_AT_MS,
            &healthy_observation()?,
        )?;
        let key = (
            ProviderIdV1::new("claude")?,
            UsageAccountIdV1::new("acct-primary")?,
        );
        let too_soon = BTreeMap::from([(key.clone(), UtcMillisV1::new(OBSERVED_AT_MS - 50_000))]);
        assert!(
            assemble(
                std::slice::from_ref(&bound),
                OBSERVED_AT_MS,
                &policy,
                &too_soon,
            )?
            .usage_snapshots
            .is_empty()
        );
        let exact = BTreeMap::from([(key, UtcMillisV1::new(OBSERVED_AT_MS - 60_000))]);
        assert_eq!(
            assemble(&[bound], OBSERVED_AT_MS, &policy, &exact)?
                .usage_snapshots
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn unusable_snapshot_with_no_provider_windows_is_excluded() -> Result {
        let bound = bind("acct-primary", 1_000_000, 1_000_000, &empty_observation()?)?;
        let input = assemble(
            &[bound],
            1_000_000,
            &multiplexer_policy()?,
            &BTreeMap::new(),
        )?;
        assert!(input.usage_snapshots.is_empty());
        Ok(())
    }

    #[test]
    fn evaluation_before_receipt_is_a_typed_assembler_error() -> Result {
        let bound = bind(
            "acct-primary",
            OBSERVED_AT_MS,
            OBSERVED_AT_MS,
            &healthy_observation()?,
        )?;
        assert!(matches!(
            assemble(
                &[bound],
                OBSERVED_AT_MS - 1,
                &multiplexer_policy()?,
                &BTreeMap::new(),
            ),
            Err(UsageMultiplexerError::EvaluationBeforeReceipt)
        ));
        Ok(())
    }

    #[test]
    fn equal_and_older_observations_are_non_monotonic_before_all_exclusions() -> Result {
        let healthy = bind(
            "acct-primary",
            OBSERVED_AT_MS,
            OBSERVED_AT_MS,
            &healthy_observation()?,
        )?;
        let unusable = bind(
            "acct-primary",
            OBSERVED_AT_MS,
            OBSERVED_AT_MS,
            &empty_observation()?,
        )?;
        let key = (
            ProviderIdV1::new("claude")?,
            UsageAccountIdV1::new("acct-primary")?,
        );
        for cooldown_ms in [1, 60_000] {
            let policy = multiplexer_policy_with_cooldown(cooldown_ms)?;
            for last_accepted_ms in [OBSERVED_AT_MS, OBSERVED_AT_MS + 1] {
                let last = BTreeMap::from([(key.clone(), UtcMillisV1::new(last_accepted_ms))]);
                for (evaluated_at_ms, bound) in [
                    (OBSERVED_AT_MS + 120_000, &healthy),
                    (OBSERVED_AT_MS, &unusable),
                ] {
                    assert!(matches!(
                        assemble(std::slice::from_ref(bound), evaluated_at_ms, &policy, &last,),
                        Err(UsageMultiplexerError::NonMonotonicObservation)
                    ));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn zero_cooldown_is_not_constructible() {
        assert!(DurationMillisV1::new(0).is_err());
    }

    #[test]
    fn observation_after_receipt_is_refused_at_the_observe_boundary() -> Result {
        let mut owner = UsageMultiplexer::new();
        let handle = enroll(&mut owner, "acct-primary")?;
        assert_eq!(
            owner.observe_claude_statusline(
                &handle,
                &empty_observation()?,
                UtcMillisV1::new(OBSERVED_AT_MS),
                UtcMillisV1::new(OBSERVED_AT_MS - 1),
            ),
            Err(TrustedUsageError::ObservationAfterReceipt)
        );
        Ok(())
    }

    #[test]
    fn provider_window_reset_boundaries_are_refused_during_observation() -> Result {
        let decoded = five_hour_observation()?;
        for observed_at_ms in [
            RESET_AT_MS,
            RESET_AT_MS + 1,
            RESET_AT_MS - FIVE_HOURS_MS - 1,
        ] {
            let mut owner = UsageMultiplexer::new();
            let handle = enroll(&mut owner, "acct-primary")?;
            assert_eq!(
                owner.observe_claude_statusline(
                    &handle,
                    &decoded,
                    UtcMillisV1::new(observed_at_ms),
                    UtcMillisV1::new(observed_at_ms),
                ),
                Err(TrustedUsageError::InvalidProviderWindowTiming)
            );
        }

        let valid = bind(
            "acct-primary",
            RESET_AT_MS - FIVE_HOURS_MS,
            RESET_AT_MS - FIVE_HOURS_MS,
            &decoded,
        )?;
        let input = assemble(
            &[valid],
            RESET_AT_MS - FIVE_HOURS_MS,
            &multiplexer_policy()?,
            &BTreeMap::new(),
        )?;
        assert_eq!(input.usage_snapshots.len(), 1);
        Ok(())
    }

    #[test]
    fn assembly_admits_exact_total_slot_per_slot_floor_and_candidate_bounds() -> Result {
        let observations =
            bound_observations(MAX_USAGE_OBSERVATION_SLOTS, MAX_RAW_OBSERVATIONS_PER_SLOT)?;
        assert_eq!(observations.len(), MAX_RAW_USAGE_OBSERVATIONS);
        let mut last_accepted = BTreeMap::new();
        for entry in 0..MAX_LAST_ACCEPTED_OBSERVATIONS {
            last_accepted.insert(
                (
                    ProviderIdV1::new("claude")?,
                    UsageAccountIdV1::new(format!("acct-history-{entry}"))?,
                ),
                UtcMillisV1::new(OBSERVED_AT_MS - 60_000),
            );
        }
        let input = assemble_routing_input(
            pending_dispatch(
                OBSERVED_AT_MS + 1_000,
                vec![fixed_candidate()?; MAX_DISPATCH_CANDIDATES],
            )?,
            &multiplexer_policy()?,
            &observations,
            &last_accepted,
        )?;
        assert_eq!(input.usage_snapshots.len(), MAX_RAW_USAGE_OBSERVATIONS);
        assert_eq!(input.candidates.len(), MAX_DISPATCH_CANDIDATES);
        Ok(())
    }

    #[test]
    fn total_overflow_is_rejected_before_any_observation_evaluation() -> Result {
        let mut observations =
            bound_observations(MAX_USAGE_OBSERVATION_SLOTS, MAX_RAW_OBSERVATIONS_PER_SLOT)?;
        observations.push(observations[0].clone());
        assert!(matches!(
            assemble(
                &observations,
                OBSERVED_AT_MS - 1,
                &multiplexer_policy()?,
                &BTreeMap::new(),
            ),
            Err(UsageMultiplexerError::TooManyRawObservations {
                maximum: MAX_RAW_USAGE_OBSERVATIONS
            })
        ));
        Ok(())
    }

    #[test]
    fn every_other_overflow_is_rejected_before_observation_evaluation() -> Result {
        let policy = multiplexer_policy()?;
        let distinct_slots = bound_observations(MAX_USAGE_OBSERVATION_SLOTS + 1, 1)?;
        assert!(matches!(
            assemble(
                &distinct_slots,
                OBSERVED_AT_MS - 1,
                &policy,
                &BTreeMap::new(),
            ),
            Err(UsageMultiplexerError::TooManyUsageObservationSlots {
                maximum: MAX_USAGE_OBSERVATION_SLOTS
            })
        ));

        let noisy_slot = bound_observations(1, MAX_RAW_OBSERVATIONS_PER_SLOT + 1)?;
        assert!(matches!(
            assemble(&noisy_slot, OBSERVED_AT_MS - 1, &policy, &BTreeMap::new(),),
            Err(UsageMultiplexerError::TooManyRawObservationsPerSlot {
                maximum: MAX_RAW_OBSERVATIONS_PER_SLOT
            })
        ));

        let legitimate = bind(
            "acct-primary",
            OBSERVED_AT_MS,
            OBSERVED_AT_MS,
            &healthy_observation()?,
        )?;
        let mut too_many_last_accepted = BTreeMap::new();
        for entry in 0..=MAX_LAST_ACCEPTED_OBSERVATIONS {
            too_many_last_accepted.insert(
                (
                    ProviderIdV1::new("claude")?,
                    UsageAccountIdV1::new(format!("acct-history-{entry}"))?,
                ),
                UtcMillisV1::new(OBSERVED_AT_MS - 60_000),
            );
        }
        assert!(matches!(
            assemble(
                std::slice::from_ref(&legitimate),
                OBSERVED_AT_MS - 1,
                &policy,
                &too_many_last_accepted,
            ),
            Err(UsageMultiplexerError::TooManyLastAcceptedObservations {
                maximum: MAX_LAST_ACCEPTED_OBSERVATIONS
            })
        ));

        assert!(matches!(
            assemble_routing_input(
                pending_dispatch(
                    OBSERVED_AT_MS - 1,
                    vec![fixed_candidate()?; MAX_DISPATCH_CANDIDATES + 1],
                )?,
                &policy,
                &[legitimate],
                &BTreeMap::new(),
            ),
            Err(UsageMultiplexerError::TooManyDispatchCandidates {
                maximum: MAX_DISPATCH_CANDIDATES
            })
        ));
        Ok(())
    }

    #[test]
    fn noisy_slot_rejection_preserves_observations_and_caller_floor_state() -> Result {
        let observations = bound_observations(1, MAX_RAW_OBSERVATIONS_PER_SLOT + 1)?;
        let last_accepted = BTreeMap::from([(
            (
                ProviderIdV1::new("claude")?,
                UsageAccountIdV1::new("acct-slot-0")?,
            ),
            UtcMillisV1::new(OBSERVED_AT_MS - 60_000),
        )]);
        let original_observations = observations.clone();
        let original_last_accepted = last_accepted.clone();
        assert!(matches!(
            assemble(
                &observations,
                OBSERVED_AT_MS - 1,
                &multiplexer_policy()?,
                &last_accepted,
            ),
            Err(UsageMultiplexerError::TooManyRawObservationsPerSlot { .. })
        ));
        assert_eq!(observations, original_observations);
        assert_eq!(last_accepted, original_last_accepted);
        Ok(())
    }

    #[test]
    fn assembly_preserves_mixed_slot_order_while_excluding_non_evidence() -> Result {
        let healthy = healthy_observation()?;
        let empty = empty_observation()?;
        let observations = vec![
            bind("acct-first", OBSERVED_AT_MS, OBSERVED_AT_MS, &healthy)?,
            bind(
                "acct-expired",
                OBSERVED_AT_MS - 240_000,
                OBSERVED_AT_MS - 240_000,
                &healthy,
            )?,
            bind(
                "acct-unusable",
                OBSERVED_AT_MS + 100,
                OBSERVED_AT_MS + 100,
                &empty,
            )?,
            bind(
                "acct-cooling",
                OBSERVED_AT_MS + 200,
                OBSERVED_AT_MS + 200,
                &healthy,
            )?,
            bind(
                "acct-last",
                OBSERVED_AT_MS + 300,
                OBSERVED_AT_MS + 300,
                &healthy,
            )?,
        ];
        let last_accepted = BTreeMap::from([(
            (
                ProviderIdV1::new("claude")?,
                UsageAccountIdV1::new("acct-cooling")?,
            ),
            UtcMillisV1::new(OBSERVED_AT_MS - 30_000),
        )]);
        let input = assemble(
            &observations,
            OBSERVED_AT_MS + 1_000,
            &multiplexer_policy()?,
            &last_accepted,
        )?;
        let accounts = input
            .usage_snapshots
            .iter()
            .map(|snapshot| snapshot.usage_account_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(accounts, vec!["acct-first", "acct-last"]);
        Ok(())
    }

    #[test]
    fn assembly_is_deterministic_and_integrates_with_real_route_decision() -> Result {
        let bound = bind(
            "acct-primary",
            OBSERVED_AT_MS,
            OBSERVED_AT_MS,
            &healthy_observation()?,
        )?;
        let policy = multiplexer_policy()?;
        let build = || {
            assemble(
                std::slice::from_ref(&bound),
                OBSERVED_AT_MS + 1_000,
                &policy,
                &BTreeMap::new(),
            )
        };
        let first = build()?;
        let second = build()?;
        assert_eq!(first, second);
        assert_eq!(first.usage_snapshots.len(), 1);
        let decision = decide_route(&fixed_policy()?, &first)?;
        assert_eq!(
            decision
                .dispatch_outcome
                .route()
                .ok_or("expected an executable route")?
                .model,
            "claude-sonnet-5"
        );
        Ok(())
    }
}
