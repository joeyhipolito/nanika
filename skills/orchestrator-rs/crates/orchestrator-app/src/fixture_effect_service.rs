//! Fixture-only artifact effect and independent evidence-attestation adapters.

use crate::{
    FixtureArtifactAttestor, FixtureArtifactAuthority, FixtureArtifactError,
    FixtureArtifactErrorKind, FixtureArtifactReceipt,
};
use orchestrator_core::{MissionId, PhaseId};
use orchestrator_exec::{
    ArtifactReceipt, ContractError, EffectBudget, EffectKind, EffectReceipt, EffectRequest,
    EffectService, EffectServiceError, EffectServiceErrorKind, EffectStatus, EvidenceVerification,
    EvidenceVerificationRequest, EvidenceVerifier, EvidenceVerifierError,
    EvidenceVerifierErrorKind, ServiceContractError,
};
use std::{
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use thiserror::Error;

/// Redacted construction failure for a fixture artifact service pair.
#[derive(Debug, Error)]
pub enum FixtureArtifactServiceBuildError {
    #[error("fixture artifact service binding does not match its exact authority")]
    BindingMismatch,
    #[error(transparent)]
    Contract(#[from] ServiceContractError),
}

struct EffectErrors {
    denied: EffectServiceError,
    invalid: EffectServiceError,
    execution: EffectServiceError,
    unavailable: EffectServiceError,
}

impl EffectErrors {
    fn new() -> Result<Self, ServiceContractError> {
        Ok(Self {
            denied: EffectServiceError::new(
                EffectServiceErrorKind::Denied,
                "fixture effect is outside its single enrolled artifact",
            )?,
            invalid: EffectServiceError::new(
                EffectServiceErrorKind::InvalidRequest,
                "fixture artifact effect does not match its exact bound input",
            )?,
            execution: EffectServiceError::new(
                EffectServiceErrorKind::Execution,
                "fixture artifact publication could not be attested",
            )?,
            unavailable: EffectServiceError::new(
                EffectServiceErrorKind::Unavailable,
                "fixture artifact effect state is unavailable",
            )?,
        })
    }
}

struct EvidenceErrors {
    missing: EvidenceVerifierError,
    mismatch: EvidenceVerifierError,
    denied: EvidenceVerifierError,
    unavailable: EvidenceVerifierError,
    invalid_expectation: EvidenceVerifierError,
}

impl EvidenceErrors {
    fn new() -> Result<Self, ServiceContractError> {
        Ok(Self {
            missing: EvidenceVerifierError::new(
                EvidenceVerifierErrorKind::Missing,
                "fixture artifact has not been durably published",
            )?,
            mismatch: EvidenceVerifierError::new(
                EvidenceVerifierErrorKind::Mismatch,
                "fixture artifact claim differs from independent attestation",
            )?,
            denied: EvidenceVerifierError::new(
                EvidenceVerifierErrorKind::Denied,
                "fixture evidence request is outside its enrolled mission or roots",
            )?,
            unavailable: EvidenceVerifierError::new(
                EvidenceVerifierErrorKind::Unavailable,
                "fixture artifact attestation is unavailable",
            )?,
            invalid_expectation: EvidenceVerifierError::new(
                EvidenceVerifierErrorKind::InvalidExpectation,
                "fixture evidence expectation is not the single enrolled artifact",
            )?,
        })
    }
}

enum ArtifactServiceState {
    Writable(FixtureArtifactAuthority),
    Publishing,
    PublishedUnattested(FixtureArtifactAttestor),
    Published {
        attestor: FixtureArtifactAttestor,
        receipt: FixtureArtifactReceipt,
    },
}

struct ArtifactServiceShared {
    state: Mutex<ArtifactServiceState>,
    mission_id: MissionId,
    phase_id: PhaseId,
    attempt: u32,
    resource: String,
    expectation: String,
    idempotency_key: String,
    worker_root: PathBuf,
    target_root: Option<PathBuf>,
    expected_input: Arc<[u8]>,
    effect_errors: EffectErrors,
    evidence_errors: EvidenceErrors,
}

/// Fixture-only `ArtifactWrite` service for one pre-enrolled artifact/key pair.
pub struct FixtureArtifactEffectService {
    shared: Arc<ArtifactServiceShared>,
}

/// Independent verifier backed by the service pair's exact read-only attestor.
///
/// The fixture file remains same-UID mutable. Its identity and exact bytes are
/// re-attested so mutation observed during attestation blocks completion; a
/// later uncoordinated write is still possible. This is not production
/// immutable publication or cryptographic closure.
pub struct FixtureEvidenceVerifier {
    shared: Arc<ArtifactServiceShared>,
}

impl FixtureArtifactEffectService {
    /// Consumes the only writable authority into a shared pair. The verifier
    /// remains `Missing` until the effect consumes that authority and installs
    /// its exact read-only attestor.
    pub fn new(
        authority: FixtureArtifactAuthority,
        exact_input: &[u8],
        target_root: Option<PathBuf>,
    ) -> Result<(Self, FixtureEvidenceVerifier), FixtureArtifactServiceBuildError> {
        if !authority.accepts_input(exact_input) {
            return Err(FixtureArtifactServiceBuildError::BindingMismatch);
        }
        let expected_input = authority.expected_input();
        let mission_id = authority.mission_id().clone();
        let phase_id = authority.phase_id().clone();
        let attempt = authority.attempt();
        let worker_root = authority.workspace_path();
        let resource = authority.resource_path().to_string_lossy().into_owned();
        let expectation = format!("artifact:{resource}");
        let idempotency_key = derived_idempotency_key(
            mission_id.as_str(),
            phase_id.as_str(),
            attempt,
            &resource,
            exact_input,
        );
        EffectRequest::new(
            EffectKind::ArtifactWrite,
            resource.clone(),
            idempotency_key.clone(),
        )?
        .with_input(exact_input.to_vec())?;
        let shared = Arc::new(ArtifactServiceShared {
            state: Mutex::new(ArtifactServiceState::Writable(authority)),
            mission_id,
            phase_id,
            attempt,
            resource,
            expectation,
            idempotency_key,
            worker_root,
            target_root,
            expected_input,
            effect_errors: EffectErrors::new()?,
            evidence_errors: EvidenceErrors::new()?,
        });
        Ok((
            Self {
                shared: Arc::clone(&shared),
            },
            FixtureEvidenceVerifier { shared },
        ))
    }

    #[must_use]
    pub fn resource(&self) -> &str {
        &self.shared.resource
    }

    #[must_use]
    pub fn expectation(&self) -> &str {
        &self.shared.expectation
    }

    /// Returns the deterministic replay key derived from the exact binding.
    /// Recomposition of the same mission/phase/attempt/path/bytes yields the
    /// same key; callers cannot silently bind a different key to an existing
    /// artifact.
    #[must_use]
    pub fn idempotency_key(&self) -> &str {
        &self.shared.idempotency_key
    }

    fn receipt(
        &self,
        fixture: &FixtureArtifactReceipt,
        status: EffectStatus,
    ) -> Result<EffectReceipt, EffectServiceError> {
        EffectReceipt::new(
            self.shared.idempotency_key.clone(),
            self.shared.idempotency_key.clone(),
            status,
            Some(fixture.digest().as_bytes().to_vec()),
        )
        .map_err(|_| self.shared.effect_errors.unavailable.clone())
    }
}

impl fmt::Debug for FixtureArtifactEffectService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureArtifactEffectService")
            .field("kind", &"single-artifact-fixture-effect-service")
            .finish()
    }
}

impl fmt::Debug for FixtureEvidenceVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureEvidenceVerifier")
            .field("kind", &"sealed-fixture-artifact-verifier")
            .finish()
    }
}

impl EffectService for FixtureArtifactEffectService {
    fn execute(
        &self,
        request: &EffectRequest,
        budget: EffectBudget<'_>,
    ) -> Result<EffectReceipt, EffectServiceError> {
        if request.kind() != EffectKind::ArtifactWrite {
            return Err(self.shared.effect_errors.denied.clone());
        }
        if request.resource() != self.shared.resource
            || request.idempotency_key() != self.shared.idempotency_key
        {
            return Err(self.shared.effect_errors.denied.clone());
        }
        if request.expose_input() != Some(self.shared.expected_input.as_ref()) {
            return Err(self.shared.effect_errors.invalid.clone());
        }

        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| self.shared.effect_errors.unavailable.clone())?;
        if let ArtifactServiceState::Published { attestor, receipt } = &*state {
            let observed = attestor
                .attest()
                .map_err(|_| self.shared.effect_errors.execution.clone())?;
            if &observed != receipt {
                return Err(self.shared.effect_errors.execution.clone());
            }
            return self.receipt(&observed, EffectStatus::AlreadyApplied);
        }
        if matches!(*state, ArtifactServiceState::PublishedUnattested(_)) {
            let attestor = match std::mem::replace(&mut *state, ArtifactServiceState::Publishing) {
                ArtifactServiceState::PublishedUnattested(attestor) => attestor,
                _ => return Err(self.shared.effect_errors.unavailable.clone()),
            };
            let receipt = match attestor.attest() {
                Ok(receipt) => receipt,
                Err(_) => {
                    *state = ArtifactServiceState::PublishedUnattested(attestor);
                    return Err(self.shared.effect_errors.execution.clone());
                }
            };
            let effect_receipt = self.receipt(&receipt, EffectStatus::AlreadyApplied)?;
            *state = ArtifactServiceState::Published { attestor, receipt };
            return Ok(effect_receipt);
        }
        if !matches!(*state, ArtifactServiceState::Writable(_)) {
            return Err(self.shared.effect_errors.unavailable.clone());
        }
        let authority = match std::mem::replace(&mut *state, ArtifactServiceState::Publishing) {
            ArtifactServiceState::Writable(authority) => authority,
            _ => return Err(self.shared.effect_errors.unavailable.clone()),
        };
        let already_present = authority.publication_exists();
        if !already_present {
            if let Err(error) = budget.admit() {
                *state = ArtifactServiceState::Writable(authority);
                return Err(error);
            }
        }
        let attestor = match authority.publish_recoverable() {
            Ok(attestor) => attestor,
            Err((authority, _error)) => {
                *state = ArtifactServiceState::Writable(authority);
                return Err(self.shared.effect_errors.execution.clone());
            }
        };
        let receipt = match attestor.attest() {
            Ok(receipt) => receipt,
            Err(_) => {
                *state = ArtifactServiceState::PublishedUnattested(attestor);
                return Err(self.shared.effect_errors.execution.clone());
            }
        };
        let status = if already_present {
            EffectStatus::AlreadyApplied
        } else {
            EffectStatus::Applied
        };
        let effect_receipt = self.receipt(&receipt, status)?;
        *state = ArtifactServiceState::Published { attestor, receipt };
        Ok(effect_receipt)
    }
}

impl EvidenceVerifier for FixtureEvidenceVerifier {
    fn verify(
        &self,
        request: &EvidenceVerificationRequest<'_>,
    ) -> Result<EvidenceVerification, EvidenceVerifierError> {
        if request.mission_id() != self.shared.mission_id.as_str()
            || request.phase_id() != self.shared.phase_id.as_str()
            || request.attempt() != self.shared.attempt
            || request.worker_root() != self.shared.worker_root
            || request.target_root() != self.shared.target_root.as_deref()
        {
            return Err(self.shared.evidence_errors.denied.clone());
        }
        if request.expected().len() != 1
            || request.expected()[0] != self.shared.expectation.as_str()
        {
            return Err(self.shared.evidence_errors.invalid_expectation.clone());
        }
        let state = self
            .shared
            .state
            .lock()
            .map_err(|_| self.shared.evidence_errors.unavailable.clone())?;
        let (attestor, receipt) = match &*state {
            ArtifactServiceState::Writable(_) => {
                return Err(self.shared.evidence_errors.missing.clone());
            }
            ArtifactServiceState::Publishing | ArtifactServiceState::PublishedUnattested(_) => {
                return Err(self.shared.evidence_errors.unavailable.clone());
            }
            ArtifactServiceState::Published { attestor, receipt } => (attestor, receipt),
        };
        let observed = attestor
            .attest()
            .map_err(|error| map_attestation_error(&self.shared.evidence_errors, &error))?;
        if &observed != receipt {
            return Err(self.shared.evidence_errors.mismatch.clone());
        }
        let claim = match request.claimed_artifacts() {
            [] => return Err(self.shared.evidence_errors.missing.clone()),
            [claim] => claim,
            _ => return Err(self.shared.evidence_errors.mismatch.clone()),
        };
        if claim.path() != observed.relative_path()
            || claim.digest() != observed.digest()
            || claim.bytes() != observed.bytes()
        {
            return Err(self.shared.evidence_errors.mismatch.clone());
        }
        let verified = ArtifactReceipt::new(
            observed.relative_path(),
            observed.digest(),
            observed.bytes(),
        )
        .map_err(|_: ContractError| self.shared.evidence_errors.unavailable.clone())?;
        EvidenceVerification::new(vec![verified])
            .map_err(|_| self.shared.evidence_errors.unavailable.clone())
    }
}

fn derived_idempotency_key(
    mission_id: &str,
    phase_id: &str,
    attempt: u32,
    resource: &str,
    exact_input: &[u8],
) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let attempt_bytes = attempt.to_be_bytes();
    for segment in [
        mission_id.as_bytes(),
        phase_id.as_bytes(),
        attempt_bytes.as_slice(),
        resource.as_bytes(),
        exact_input,
    ] {
        for byte in segment {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fixture-artifact-v1-{hash:016x}")
}

fn map_attestation_error(
    errors: &EvidenceErrors,
    error: &FixtureArtifactError,
) -> EvidenceVerifierError {
    match error.kind() {
        FixtureArtifactErrorKind::Missing => errors.missing.clone(),
        FixtureArtifactErrorKind::InvalidAttempt
        | FixtureArtifactErrorKind::InvalidName
        | FixtureArtifactErrorKind::TooLarge => errors.invalid_expectation.clone(),
        FixtureArtifactErrorKind::FixtureOnly => errors.denied.clone(),
        FixtureArtifactErrorKind::IdentityChanged
        | FixtureArtifactErrorKind::Conflict
        | FixtureArtifactErrorKind::InvalidEntry
        | FixtureArtifactErrorKind::ContentMismatch => errors.mismatch.clone(),
        FixtureArtifactErrorKind::Filesystem => errors.unavailable.clone(),
    }
}
