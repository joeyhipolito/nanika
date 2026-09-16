//! Provider-neutral continuation capsule and same-provider resume decision.
//!
//! After an attempt fails, is interrupted, or completes, the engine must decide
//! whether the next attempt **resumes the same provider session** or **starts a
//! fresh session cross-provider from a provider-neutral capsule**, do so without
//! repeating any accepted effect, and make that decision byte-identically
//! replayable offline.
//!
//! Two disjoint outcomes are modelled by [`ContinuationDecision`]:
//!
//! * [`ContinuationDecision::ResumeSameProvider`] reuses the existing
//!   [`SessionHandle`] — the opaque, family-bound handle already minted by the
//!   executor contract. Its two fail-closed cross-family checks
//!   (`ExecutionRequest::new` and `ResolvedExecutor::execute`) are the sole
//!   enforcement loci; this module invents no second handle and no second check.
//! * [`ContinuationDecision::FreshFromCapsule`] carries a
//!   [`ContinuationCapsule`]: a typed-only, provider-neutral record whose every
//!   field is a closed id, a small integer, a bounded plan-derived label, or a
//!   SHA-256 digest. There is deliberately no `String` blob, no
//!   `serde_json::Value`, and — critically — **no `session_id`**, so a provider
//!   session id can never travel in the capsule and raw provider output /
//!   chain-of-thought is structurally unrepresentable, the same discipline as
//!   [`crate::runtime_store::ReasoningWrite`].

use orchestrator_core::{MissionId, PhaseId};
use orchestrator_exec::{ContractError, RuntimeFamily, SessionHandle};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::mission_service::AdmittedMission;

/// Longest bounded, plan-derived label carried by a capsule field. Mirrors the
/// reasoning family's `MAX_REASONING_TEXT_BYTES` ceiling so capsule labels and
/// reasoning labels share one bound.
const CONTINUATION_LABEL_BYTES: usize = 4096;

/// Additive schema tag for the capsule. Mirrors the
/// `TERMINAL_DECISION_SCHEMA_VERSION` idiom: it versions only the capsule's
/// field shape, independent of any store or journal version.
pub const CONTINUATION_CAPSULE_SCHEMA_VERSION: u8 = 1;

/// Structured failures of the continuation layer.
#[derive(Debug, Error)]
pub enum ContinuationError {
    /// The phase named for the capsule is not part of the admitted mission.
    #[error("phase {0} is not part of the admitted mission")]
    UnknownPhase(String),
    /// A capsule ordinal (attempt / revision) was zero.
    #[error("continuation {field} must be positive")]
    ZeroOrdinal {
        /// Field whose ordinal was zero.
        field: &'static str,
    },
    /// The phase's persisted runtime family could not be parsed.
    #[error("phase runtime family is invalid: {0}")]
    Runtime(#[from] ContractError),
    /// A retry candidate repeats a strategy already attempted for this phase.
    #[error("retry candidate repeats an already-attempted strategy")]
    StrategyUnchanged,
}

/// A digested inter-phase handoff. Carries only the producing/consuming phase
/// ids and a SHA-256 digest of the handoff summary — never the upstream
/// provider output the summary was derived from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandoffDigest {
    /// Producing phase.
    pub from_phase: PhaseId,
    /// Consuming phase.
    pub to_phase: PhaseId,
    /// SHA-256 hex digest of the handoff summary.
    pub summary_digest: String,
}

/// A typed pointer into upstream evidence. The artifact itself is referenced by
/// digest, never inlined.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceDigest {
    /// Phase the evidence belongs to.
    pub phase_id: PhaseId,
    /// Evidence kind tag.
    pub kind: String,
    /// SHA-256 hex digest of the evidence.
    pub digest: String,
}

/// A provider-neutral seed for a fresh cross-provider session.
///
/// Every field is a closed id, a small integer, a bounded plan-derived label,
/// or a SHA-256 digest. There is no free-form text field and no `session_id`,
/// so a fresh session inherits the *task* (objective, assignment, upstream
/// handoffs, what strategies already failed) without ever carrying the prior
/// session's provider id or any raw transcript.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContinuationCapsule {
    schema_version: u8,
    mission_id: MissionId,
    phase_id: PhaseId,
    attempt: u32,
    revision: u32,
    objective: String,
    persona: String,
    role: String,
    model_tier: String,
    runtime: RuntimeFamily,
    selection_method: String,
    dependency_handoffs: Vec<HandoffDigest>,
    evidence_refs: Vec<EvidenceDigest>,
    prior_strategy_fingerprints: Vec<String>,
}

impl ContinuationCapsule {
    /// Builds a capsule from the admitted plan for the phase's next attempt.
    ///
    /// The capsule is derived solely from the compiled plan (objective, persona
    /// assignment, dependency structure) and the caller-supplied prior strategy
    /// fingerprints. The terminal outcome's raw/partial output is never an input
    /// here, so no provider text can enter the capsule.
    ///
    /// # Errors
    ///
    /// Returns [`ContinuationError`] if the phase is unknown to the mission, an
    /// ordinal is zero, or the phase's runtime family does not parse.
    pub fn from_admitted(
        mission: &AdmittedMission,
        phase_id: &PhaseId,
        attempt: u32,
        revision: u32,
        prior_strategy_fingerprints: Vec<String>,
    ) -> Result<Self, ContinuationError> {
        if attempt == 0 {
            return Err(ContinuationError::ZeroOrdinal { field: "attempt" });
        }
        if revision == 0 {
            return Err(ContinuationError::ZeroOrdinal { field: "revision" });
        }
        let phase = mission
            .plan()
            .phases
            .iter()
            .find(|phase| &phase.id == phase_id)
            .ok_or_else(|| ContinuationError::UnknownPhase(phase_id.as_str().to_owned()))?;

        let mut dependency_handoffs = Vec::new();
        for dependency in &phase.dependencies {
            dependency_handoffs.push(HandoffDigest {
                from_phase: dependency.clone(),
                to_phase: phase.id.clone(),
                summary_digest: sha256_hex(&format!(
                    "{} feeds {}",
                    dependency.as_str(),
                    phase.name
                )),
            });
        }

        let evidence_refs = vec![EvidenceDigest {
            phase_id: phase.id.clone(),
            kind: "objective".to_owned(),
            digest: sha256_hex(&phase.objective),
        }];

        Ok(Self {
            schema_version: CONTINUATION_CAPSULE_SCHEMA_VERSION,
            mission_id: mission.mission_id().clone(),
            phase_id: phase.id.clone(),
            attempt,
            revision,
            objective: bounded(&phase.objective),
            persona: bounded(&phase.persona),
            role: bounded(&phase.role),
            model_tier: bounded(&phase.model_tier),
            runtime: RuntimeFamily::parse(phase.runtime.clone())?,
            selection_method: bounded(&phase.persona_selection_method),
            dependency_handoffs,
            evidence_refs,
            prior_strategy_fingerprints,
        }
        .validated())
    }

    fn validated(mut self) -> Self {
        // Bound every plan-derived label; digests and fingerprints are fixed-
        // width hashes and need no bounding.
        self.objective = bounded(&self.objective);
        self.persona = bounded(&self.persona);
        self.role = bounded(&self.role);
        self.model_tier = bounded(&self.model_tier);
        self.selection_method = bounded(&self.selection_method);
        self
    }

    /// The mission this capsule seeds.
    #[must_use]
    pub const fn mission_id(&self) -> &MissionId {
        &self.mission_id
    }

    /// The phase this capsule seeds.
    #[must_use]
    pub const fn phase_id(&self) -> &PhaseId {
        &self.phase_id
    }

    /// The 1-based attempt this capsule seeds.
    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    /// The plan revision this capsule reflects.
    #[must_use]
    pub const fn revision(&self) -> u32 {
        self.revision
    }

    /// The provider family the fresh session will run under.
    #[must_use]
    pub const fn runtime(&self) -> &RuntimeFamily {
        &self.runtime
    }

    /// The prior strategy fingerprints already attempted for this phase.
    #[must_use]
    pub fn prior_strategy_fingerprints(&self) -> &[String] {
        &self.prior_strategy_fingerprints
    }

    /// A stable SHA-256 hex digest binding every capsule field. Deterministic
    /// across processes and clocks: identical capsules digest identically, so a
    /// persisted `capsule_digest` replays byte-for-byte.
    #[must_use]
    pub fn canonical_digest(&self) -> String {
        let mut digest = Sha256::new();
        frame(&mut digest, b"nanika:continuation-capsule:v1");
        frame(&mut digest, &[self.schema_version]);
        frame(&mut digest, self.mission_id.as_str().as_bytes());
        frame(&mut digest, self.phase_id.as_str().as_bytes());
        frame(&mut digest, &self.attempt.to_be_bytes());
        frame(&mut digest, &self.revision.to_be_bytes());
        frame(&mut digest, self.objective.as_bytes());
        frame(&mut digest, self.persona.as_bytes());
        frame(&mut digest, self.role.as_bytes());
        frame(&mut digest, self.model_tier.as_bytes());
        frame(&mut digest, self.runtime.as_str().as_bytes());
        frame(&mut digest, self.selection_method.as_bytes());
        for handoff in &self.dependency_handoffs {
            frame(&mut digest, handoff.from_phase.as_str().as_bytes());
            frame(&mut digest, handoff.to_phase.as_str().as_bytes());
            frame(&mut digest, handoff.summary_digest.as_bytes());
        }
        for evidence in &self.evidence_refs {
            frame(&mut digest, evidence.phase_id.as_str().as_bytes());
            frame(&mut digest, evidence.kind.as_bytes());
            frame(&mut digest, evidence.digest.as_bytes());
        }
        for fingerprint in &self.prior_strategy_fingerprints {
            frame(&mut digest, fingerprint.as_bytes());
        }
        hex(digest.finalize().as_slice())
    }

    /// Every plan-derived text field, for a seeded-secret scan proving no raw
    /// provider output leaked into the capsule. Digests and fingerprints are
    /// fixed-width hashes and are excluded — they cannot carry text.
    #[must_use]
    pub fn text_fields(&self) -> Vec<String> {
        let mut fields = vec![
            self.objective.clone(),
            self.persona.clone(),
            self.role.clone(),
            self.model_tier.clone(),
            self.runtime.as_str().to_owned(),
            self.selection_method.clone(),
        ];
        for evidence in &self.evidence_refs {
            fields.push(evidence.kind.clone());
        }
        fields
    }
}

/// A same-provider resume handle or a cross-provider fresh-session capsule —
/// never both.
#[derive(Clone, Debug)]
pub enum ContinuationDecision {
    /// Resume the same provider session under its family-bound handle.
    ResumeSameProvider {
        /// The opaque, family-bound handle to resume into.
        handle: SessionHandle,
    },
    /// Start a fresh session (cross-provider or no live session) from a
    /// provider-neutral capsule.
    FreshFromCapsule {
        /// The provider-neutral seed for the fresh session. Boxed because it is
        /// far larger than the [`SessionHandle`] carried by the other variant.
        capsule: Box<ContinuationCapsule>,
    },
}

/// A provider-neutral, replay-stable view of one persisted continuation
/// decision. This is the offline-replay read surface: reconstructing it over
/// the durable journal rows reproduces the original resume-vs-fresh choice
/// without re-running any provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContinuationDecisionSummary {
    /// The 1-based attempt this decision seeded.
    pub attempt: u32,
    /// The plan revision the decision reflected.
    pub revision: u32,
    /// Whether a same-provider session resume was selected.
    pub resumed_same_provider: bool,
    /// The provider family the next attempt runs under.
    pub chosen_runtime: String,
    /// The capsule digest for a fresh session, or `None` for a resume.
    pub capsule_digest: Option<String>,
}

/// The outcome of a semantic-failure retry decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryPlan {
    /// Retry with a materially different strategy.
    Retry,
    /// No materially different strategy remains within the current DAG; the
    /// plan must be durably revised or paused for approval.
    ReplanRequired,
}

/// Selects same-provider resume iff a durable handle exists for the exact prior
/// attempt **and** the next runtime equals that handle's family; otherwise a
/// fresh session from the capsule.
///
/// This is a pure function of durable inputs (the persisted handle) and the
/// resolved next runtime — no clock, no live probe, no operator flag — so it
/// replays offline byte-for-byte. Cross-family reuse is impossible: a handle
/// whose family differs from `next_runtime` is never selected, and even if a
/// caller forced it, [`orchestrator_exec::ExecutionRequest::new`] rejects it
/// fail-closed.
#[must_use]
pub fn select_resume_or_fresh(
    persisted_handle: Option<SessionHandle>,
    next_runtime: &RuntimeFamily,
    capsule: ContinuationCapsule,
) -> ContinuationDecision {
    match persisted_handle {
        Some(handle) if handle.can_resume_into(next_runtime) => {
            ContinuationDecision::ResumeSameProvider { handle }
        }
        _ => ContinuationDecision::FreshFromCapsule {
            capsule: Box::new(capsule),
        },
    }
}

/// Enforces "a semantic-failure retry needs a materially different strategy".
///
/// * `None` candidate → the strategy ladder is exhausted → [`RetryPlan::ReplanRequired`].
/// * A candidate whose fingerprint collides with any prior attempt →
///   [`ContinuationError::StrategyUnchanged`] (a cosmetic retry is refused).
/// * A materially different candidate → [`RetryPlan::Retry`].
///
/// # Errors
///
/// Returns [`ContinuationError::StrategyUnchanged`] when the candidate repeats a
/// prior strategy.
pub fn select_retry(
    prior_strategy_fingerprints: &[String],
    candidate_fingerprint: Option<&str>,
) -> Result<RetryPlan, ContinuationError> {
    match candidate_fingerprint {
        None => Ok(RetryPlan::ReplanRequired),
        Some(candidate) => {
            if prior_strategy_fingerprints
                .iter()
                .any(|prior| prior == candidate)
            {
                Err(ContinuationError::StrategyUnchanged)
            } else {
                Ok(RetryPlan::Retry)
            }
        }
    }
}

/// Derives a strategy fingerprint from the strategy **inputs**, never the
/// attempt ordinal.
///
/// Folding in the attempt ordinal (the pre-B2 behaviour) made every retry's
/// fingerprint unique by construction, so an unchanged strategy could never be
/// detected. Deriving from `mission | phase | revision | runtime | model_tier |
/// persona | role` instead means a cosmetic retry (same inputs) collides with a
/// prior fingerprint — the signal [`select_retry`] needs — while a materially
/// different retry (escalated tier / model / persona / bumped revision) yields a
/// new fingerprint.
#[must_use]
pub fn strategy_fingerprint(
    mission_id: &MissionId,
    phase_id: &PhaseId,
    revision: u32,
    runtime: &str,
    model_tier: &str,
    persona: &str,
    role: &str,
) -> String {
    let mut digest = Sha256::new();
    frame(&mut digest, b"nanika:continuation-strategy-fingerprint:v1");
    frame(&mut digest, mission_id.as_str().as_bytes());
    frame(&mut digest, phase_id.as_str().as_bytes());
    frame(&mut digest, &revision.to_be_bytes());
    frame(&mut digest, runtime.as_bytes());
    frame(&mut digest, model_tier.as_bytes());
    frame(&mut digest, persona.as_bytes());
    frame(&mut digest, role.as_bytes());
    hex(digest.finalize().as_slice())
}

/// Truncates text to a bounded, char-boundary-safe capsule label.
fn bounded(value: &str) -> String {
    if value.len() <= CONTINUATION_LABEL_BYTES {
        return value.to_owned();
    }
    let mut end = CONTINUATION_LABEL_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// Length-framed digest update, so concatenation is unambiguous.
fn frame(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
}

fn sha256_hex(input: &str) -> String {
    hex(Sha256::digest(input.as_bytes()).as_slice())
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}
