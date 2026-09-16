//! Untrusted mission-proposal model and its deterministic DAG compiler.
//!
//! A proposal is arbitrary caller data — from a provider or the offline keyword
//! fallback ([`crate::mission::keyword_fallback_proposal`]). Nothing in it is
//! trusted until [`compile_proposal`] validates it into a typed dependency DAG.
//! Compilation is pure, side-effect-free, and deterministic: it performs no I/O,
//! reads no clock, and fails closed with a named [`ProposalError`]. Structural
//! checks (empty, oversize, missing field, duplicate name, self-dependency,
//! unknown dependency) are done by phase name so the errors name the offending
//! phase; the one non-trivial check — cycle detection — reuses the reducer's
//! proven detector so the DAG has a single source of truth.

use crate::PhaseId;
use crate::mission::{
    AuthoredParseContext, AuthoredPhase, ExecutionMode, MAX_PHASES, derive_execution_mode,
    phase_policy, resolve_persona,
};
use crate::reducer::{MissionStateBuildError, PhaseDefinition, detect_cycle};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// Maximum accepted byte length of a serialized proposal capsule.
///
/// A legitimate proposal is at most [`MAX_PHASES`] phases of bounded text, so
/// this cap sits far above any real capsule while bounding untrusted input
/// before it reaches the JSON parser. Mirrors the byte-budget idiom used by the
/// runtime store for durable JSON payloads.
pub const MAX_PROPOSAL_BYTES: usize = 64 * 1024;

/// Untrusted proposal as received from a provider or the fallback decomposer.
///
/// Field values are arbitrary caller data; nothing here is trusted until
/// [`compile_proposal`]. The type is deliberately `Deserialize`-only — it must
/// never be serialized back into a durable store, keeping raw proposal text out
/// of persistence.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct MissionProposal {
    /// Proposed phases in author-intended order.
    #[serde(default)]
    pub phases: Vec<ProposedPhase>,
}

/// One untrusted phase in a [`MissionProposal`].
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ProposedPhase {
    /// Author-facing unique key, resolved to a `phase-N` id at compile time.
    #[serde(default)]
    pub name: String,
    /// Required, non-empty concrete deliverable.
    #[serde(default)]
    pub objective: String,
    /// Requested persona; resolved against the catalog/policy at compile time.
    #[serde(default)]
    pub persona: String,
    /// Requested skills; blank entries are dropped at compile time.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Dependency phase names, resolved to ids during compile.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

impl MissionProposal {
    /// Parses a JSON proposal capsule, failing closed on oversize or malformed
    /// input.
    ///
    /// # Errors
    ///
    /// Returns [`ProposalError::Malformed`] when the input exceeds
    /// [`MAX_PROPOSAL_BYTES`] or is not a valid proposal document.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, ProposalError> {
        if bytes.len() > MAX_PROPOSAL_BYTES {
            return Err(ProposalError::Malformed);
        }
        serde_json::from_slice(bytes).map_err(|_| ProposalError::Malformed)
    }
}

/// A validated, dispatchable plan compiled from an untrusted proposal.
///
/// Its phases carry resolved persona/tier/role/runtime and are indistinguishable
/// from an authored plan downstream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledPlan {
    /// Compiled phases in proposal order, each with a resolved `phase-N` id.
    pub phases: Vec<AuthoredPhase>,
    /// Derived execution mode, using the same rule as authored plans.
    pub execution_mode: ExecutionMode,
}

/// A named reason an untrusted proposal could not be compiled into a DAG.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ProposalError {
    /// Parse failure or a capsule over [`MAX_PROPOSAL_BYTES`].
    #[error("proposal input was malformed or exceeded the size budget")]
    Malformed,
    /// The proposal listed no phases.
    #[error("proposal contains no phases")]
    Empty,
    /// A phase was missing a required field.
    #[error("phase {index} is missing a required {field}")]
    MissingField {
        /// Zero-based index of the offending phase.
        index: usize,
        /// Name of the missing field.
        field: &'static str,
    },
    /// Two phases shared a name.
    #[error("duplicate phase name {name}")]
    DuplicateName {
        /// The repeated phase name.
        name: String,
    },
    /// A phase depended on a name that is not another phase in the proposal.
    #[error("phase {name} depends on unknown phase {dependency}")]
    UnknownDependency {
        /// The dependent phase.
        name: String,
        /// The unresolved dependency name.
        dependency: String,
    },
    /// A phase listed itself as a dependency.
    #[error("phase {name} lists a self-dependency")]
    SelfDependency {
        /// The self-referential phase.
        name: String,
    },
    /// The dependency graph contains a cycle.
    #[error("dependency cycle includes phase {name}")]
    Cycle {
        /// A phase on the detected cycle.
        name: String,
    },
    /// The proposal exceeded the phase-count limit.
    #[error("proposal exceeds the {max}-phase limit")]
    TooManyPhases {
        /// The accepted maximum ([`MAX_PHASES`]).
        max: usize,
    },
}

/// Deterministically compiles an untrusted proposal into a typed dependency DAG.
///
/// Pure and side-effect-free. Validation fails closed with the first applicable
/// named [`ProposalError`]; a rejected proposal yields no [`CompiledPlan`], so a
/// malformed or cyclic proposal can never be dispatched.
///
/// # Errors
///
/// Returns a [`ProposalError`] describing the first structural or graph
/// violation encountered.
pub fn compile_proposal(
    proposal: &MissionProposal,
    context: &AuthoredParseContext,
) -> Result<CompiledPlan, ProposalError> {
    if proposal.phases.is_empty() {
        return Err(ProposalError::Empty);
    }
    if proposal.phases.len() > MAX_PHASES {
        return Err(ProposalError::TooManyPhases { max: MAX_PHASES });
    }

    // Pass 1: validate identity, assign positional ids, reject duplicate names.
    // Recording every accepted name up front lets pass 2 resolve dependencies
    // against the full graph so a genuine cycle reaches the reducer's detector.
    let mut name_to_id: BTreeMap<String, PhaseId> = BTreeMap::new();
    let mut id_to_name: BTreeMap<PhaseId, String> = BTreeMap::new();
    let mut ids: Vec<PhaseId> = Vec::with_capacity(proposal.phases.len());
    for (index, phase) in proposal.phases.iter().enumerate() {
        if phase.name.trim().is_empty() {
            return Err(ProposalError::MissingField {
                index,
                field: "name",
            });
        }
        if phase.objective.trim().is_empty() {
            return Err(ProposalError::MissingField {
                index,
                field: "objective",
            });
        }
        let id =
            PhaseId::new(format!("phase-{}", index + 1)).map_err(|_| ProposalError::Malformed)?;
        if name_to_id.insert(phase.name.clone(), id.clone()).is_some() {
            return Err(ProposalError::DuplicateName {
                name: phase.name.clone(),
            });
        }
        id_to_name.insert(id.clone(), phase.name.clone());
        ids.push(id);
    }

    // Pass 2: resolve dependencies and policy, building typed phases and the
    // definitions fed to the shared cycle detector.
    let mut phases = Vec::with_capacity(proposal.phases.len());
    let mut definitions = Vec::with_capacity(proposal.phases.len());
    for (index, phase) in proposal.phases.iter().enumerate() {
        let id = ids[index].clone();
        let mut dependencies = Vec::new();
        let mut seen = BTreeSet::new();
        for dependency_name in &phase.depends_on {
            if dependency_name == &phase.name {
                return Err(ProposalError::SelfDependency {
                    name: phase.name.clone(),
                });
            }
            let Some(dependency_id) = name_to_id.get(dependency_name) else {
                return Err(ProposalError::UnknownDependency {
                    name: phase.name.clone(),
                    dependency: dependency_name.clone(),
                });
            };
            if seen.insert(dependency_id.clone()) {
                dependencies.push(dependency_id.clone());
            }
        }

        let policy = phase_policy(context, &phase.name);
        let (persona, persona_selection_method) = resolve_persona(context, policy, &phase.persona);
        let skills = phase
            .skills
            .iter()
            .map(|skill| skill.trim())
            .filter(|skill| !skill.is_empty())
            .map(str::to_owned)
            .collect();

        definitions.push(PhaseDefinition {
            id: id.clone(),
            dependencies: dependencies.clone(),
        });
        phases.push(AuthoredPhase {
            id,
            name: phase.name.clone(),
            objective: phase.objective.clone(),
            persona,
            persona_selection_method,
            model_tier: policy.tier.clone(),
            role: policy.role.clone(),
            skills,
            dependencies,
            expected: String::new(),
            workdir: String::new(),
            runtime: policy.runtime.clone(),
            runtime_policy_applied: true,
            stall_timeout: None,
            priority: String::new(),
            status: "pending".to_owned(),
        });
    }

    // Cycle detection is the single source of DAG truth. Structural violations
    // are already rejected by name above, so in practice only `DependencyCycle`
    // returns here; the mapper stays total for robustness.
    detect_cycle(&definitions).map_err(|error| map_state_build_error(error, &id_to_name))?;

    let execution_mode = derive_execution_mode(&phases);
    Ok(CompiledPlan {
        phases,
        execution_mode,
    })
}

/// Translates a reducer build error into a name-based [`ProposalError`].
fn map_state_build_error(
    error: MissionStateBuildError,
    id_to_name: &BTreeMap<PhaseId, String>,
) -> ProposalError {
    let name_of = |phase_id: &PhaseId| {
        id_to_name
            .get(phase_id)
            .cloned()
            .unwrap_or_else(|| phase_id.to_string())
    };
    match error {
        MissionStateBuildError::EmptyPlan => ProposalError::Empty,
        MissionStateBuildError::DuplicatePhase { phase_id } => ProposalError::DuplicateName {
            name: name_of(&phase_id),
        },
        MissionStateBuildError::UnknownDependency {
            phase_id,
            dependency,
        } => ProposalError::UnknownDependency {
            name: name_of(&phase_id),
            dependency: name_of(&dependency),
        },
        // The compiler dedupes dependencies before the detector runs, so this
        // arm is unreachable; collapse it to keep the mapping total.
        MissionStateBuildError::DuplicateDependency {
            phase_id,
            dependency,
        } => ProposalError::UnknownDependency {
            name: name_of(&phase_id),
            dependency: name_of(&dependency),
        },
        MissionStateBuildError::DependencyCycle { phase_id } => ProposalError::Cycle {
            name: name_of(&phase_id),
        },
    }
}
