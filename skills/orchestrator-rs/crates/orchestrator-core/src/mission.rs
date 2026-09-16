use crate::mission_proposal::{MissionProposal, ProposedPhase};
use crate::{PhaseId, configuration::parse_go_duration};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    time::Duration,
};
use thiserror::Error;

pub(crate) const MAX_PHASES: usize = 12;
const SUPPORTED_RUNTIMES: [&str; 7] = [
    "claude",
    "codex",
    "both",
    "anthropic-api",
    "openai-api",
    "openrouter",
    "gemini-api",
];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PersonaCatalog {
    pub names: BTreeSet<String>,
}

/// All nondeterministic persona/tier/role/runtime decisions for one phase.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhasePolicyFixture {
    pub fallback_persona: String,
    pub fallback_selection_method: String,
    pub tier: String,
    pub role: String,
    pub runtime: String,
}

impl Default for PhasePolicyFixture {
    fn default() -> Self {
        Self {
            fallback_persona: "generalist".to_owned(),
            fallback_selection_method: "fixture".to_owned(),
            tier: "work".to_owned(),
            role: "implementer".to_owned(),
            runtime: "claude".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimePolicyFixture {
    pub default: PhasePolicyFixture,
    /// Overrides keyed by authored phase name.
    pub phases: BTreeMap<String, PhasePolicyFixture>,
}

/// Target-derived fixture overrides, keyed by authored phase name.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TargetContextFixture {
    pub phases: BTreeMap<String, PhasePolicyFixture>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AuthoredParseContext {
    pub user_home: Option<PathBuf>,
    pub persona_catalog: PersonaCatalog,
    pub target_context: Option<TargetContextFixture>,
    pub policy: RuntimePolicyFixture,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionMode {
    Sequential,
    Parallel,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredPlanProjection {
    pub phases: Vec<AuthoredPhase>,
    pub execution_mode: ExecutionMode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredPhase {
    pub id: PhaseId,
    pub name: String,
    pub objective: String,
    pub persona: String,
    pub persona_selection_method: String,
    pub model_tier: String,
    pub role: String,
    pub skills: Vec<String>,
    pub dependencies: Vec<PhaseId>,
    pub expected: String,
    pub workdir: String,
    pub runtime: String,
    pub runtime_policy_applied: bool,
    pub stall_timeout: Option<Duration>,
    pub priority: String,
    pub status: String,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum MissionParseError {
    #[error("no phases parsed from authored mission")]
    NoPhases,
    #[error("duplicate phase id {0}")]
    DuplicatePhaseId(String),
}

/// Parses only authored pipe-delimited `PHASE:` records, with no ambient I/O.
pub fn parse_authored_phases(
    source: &str,
    context: &AuthoredParseContext,
) -> Result<AuthoredPlanProjection, MissionParseError> {
    let mut phases = Vec::new();
    let mut accepted_names = BTreeSet::new();
    let mut name_to_id = BTreeMap::new();

    for raw_line in source.lines() {
        let line = raw_line.trim().trim_matches('`');
        if !line.starts_with("PHASE:") {
            continue;
        }
        let mut fields = BTreeMap::new();
        for part in line.split('|') {
            let Some((key, value)) = part.split_once(':') else {
                continue;
            };
            let key = key.trim();
            if !key.is_empty() {
                fields.insert(key, value.trim());
            }
        }
        let Some(name) = fields.get("PHASE").filter(|value| !value.is_empty()) else {
            continue;
        };
        let Some(objective) = fields.get("OBJECTIVE").filter(|value| !value.is_empty()) else {
            continue;
        };
        if !accepted_names.insert((*name).to_owned()) {
            continue;
        }

        let policy = phase_policy(context, name);
        let authored_persona = fields.get("PERSONA").copied().unwrap_or_default();
        let (persona, persona_selection_method) =
            resolve_persona(context, policy, authored_persona);

        let id = PhaseId::new(format!("phase-{}", phases.len() + 1))
            .map_err(|_| MissionParseError::NoPhases)?;
        // Go records the accepted name before resolving DEPENDS, so an authored
        // self-reference resolves while forward and unknown names do not.
        name_to_id.insert((*name).to_owned(), id.clone());
        let runtime = fields
            .get("RUNTIME")
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| SUPPORTED_RUNTIMES.contains(&value.as_str()));
        let runtime_policy_applied = runtime.is_none();

        let skills = fields.get("SKILLS").map_or_else(Vec::new, |value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect()
        });
        let dependencies = fields.get("DEPENDS").map_or_else(Vec::new, |value| {
            value
                .split(',')
                .map(str::trim)
                .filter_map(|dependency| name_to_id.get(dependency).cloned())
                .collect()
        });
        let workdir = expand_home(
            fields.get("WORKDIR").copied().unwrap_or_default(),
            context.user_home.as_ref(),
        );
        let stall_timeout = fields
            .get("TIMEOUT")
            .and_then(|value| parse_go_duration(value));

        phases.push(AuthoredPhase {
            id,
            name: (*name).to_owned(),
            objective: (*objective).to_owned(),
            persona,
            persona_selection_method,
            model_tier: policy.tier.clone(),
            role: policy.role.clone(),
            skills,
            dependencies,
            expected: fields
                .get("EXPECTED")
                .copied()
                .unwrap_or_default()
                .to_owned(),
            workdir,
            runtime: runtime.unwrap_or_else(|| policy.runtime.clone()),
            runtime_policy_applied,
            stall_timeout,
            priority: fields
                .get("PRIORITY")
                .copied()
                .unwrap_or_default()
                .trim()
                .to_ascii_uppercase(),
            status: "pending".to_owned(),
        });
        if phases.len() == MAX_PHASES {
            break;
        }
    }
    if phases.is_empty() {
        return Err(MissionParseError::NoPhases);
    }
    let execution_mode = derive_execution_mode(&phases);
    Ok(AuthoredPlanProjection {
        phases,
        execution_mode,
    })
}

/// Resolves the per-phase policy fixture for an authored or proposed phase name.
///
/// Precedence mirrors the authored parser: a target-context override wins, then
/// a policy override for the name, then the default policy.
pub(crate) fn phase_policy<'context>(
    context: &'context AuthoredParseContext,
    name: &str,
) -> &'context PhasePolicyFixture {
    context
        .target_context
        .as_ref()
        .and_then(|target| target.phases.get(name))
        .or_else(|| context.policy.phases.get(name))
        .unwrap_or(&context.policy.default)
}

/// Resolves persona identity for a phase.
///
/// An authored persona wins when it is a known catalog name; otherwise the
/// policy fallback persona and its selection method apply.
pub(crate) fn resolve_persona(
    context: &AuthoredParseContext,
    policy: &PhasePolicyFixture,
    authored_persona: &str,
) -> (String, String) {
    if context.persona_catalog.names.contains(authored_persona) {
        (authored_persona.to_owned(), "llm".to_owned())
    } else {
        (
            policy.fallback_persona.clone(),
            policy.fallback_selection_method.clone(),
        )
    }
}

fn expand_home(value: &str, home: Option<&PathBuf>) -> String {
    let Some(remainder) = value.strip_prefix("~/") else {
        return value.to_owned();
    };
    home.map_or_else(
        || value.to_owned(),
        |home| home.join(remainder).to_string_lossy().into_owned(),
    )
}

pub(crate) fn derive_execution_mode(phases: &[AuthoredPhase]) -> ExecutionMode {
    if phases.len() < 2 {
        return ExecutionMode::Sequential;
    }
    if phases
        .iter()
        .skip(1)
        .any(|phase| phase.dependencies.is_empty())
    {
        return ExecutionMode::Parallel;
    }
    let mut dependency_sets = BTreeSet::new();
    for phase in phases.iter().filter(|phase| !phase.dependencies.is_empty()) {
        let ordered = phase
            .dependencies
            .iter()
            .map(PhaseId::as_str)
            .collect::<Vec<_>>()
            .join(",");
        if !dependency_sets.insert(ordered) {
            return ExecutionMode::Parallel;
        }
    }
    ExecutionMode::Sequential
}

/// Rejects duplicate IDs in caller-constructed plans before reduction or dispatch.
pub fn validate_phase_ids(phases: &[AuthoredPhase]) -> Result<(), MissionParseError> {
    let mut seen = BTreeSet::new();
    for phase in phases {
        if !seen.insert(phase.id.as_str()) {
            return Err(MissionParseError::DuplicatePhaseId(phase.id.to_string()));
        }
    }
    Ok(())
}

/// Deterministic, offline last-resort decomposer.
///
/// Emits an untrusted [`MissionProposal`] that always compiles: the same `task`
/// string yields a byte-identical proposal because it uses no clock, map
/// iteration order, or randomness. It is the fallback of last resort — authored
/// `PHASE:` records and any live provider proposal both take precedence.
///
/// Persona is left empty so
/// [`compile_proposal`](crate::mission_proposal::compile_proposal) resolves it
/// from the parse context's policy, keeping persona resolution in one place. The
/// keyword rules are intentionally minimal; this guarantees an offline run a
/// compilable plan, it is not a substitute for real decomposition.
pub fn keyword_fallback_proposal(task: &str) -> MissionProposal {
    let lowered = task.to_lowercase();
    let trimmed = task.trim();
    let summary = if trimmed.is_empty() {
        "the requested task"
    } else {
        trimmed
    };
    let matches_any = |keywords: &[&str]| keywords.iter().any(|keyword| lowered.contains(keyword));

    if matches_any(&[
        "research",
        "investigate",
        "explore",
        "analyze",
        "analyse",
        "survey",
    ]) {
        return MissionProposal {
            phases: vec![ProposedPhase {
                name: "research".to_owned(),
                objective: format!("Research {summary}"),
                persona: String::new(),
                skills: Vec::new(),
                depends_on: Vec::new(),
            }],
        };
    }

    let mut phases = vec![ProposedPhase {
        name: "implement".to_owned(),
        objective: format!("Implement {summary}"),
        persona: String::new(),
        skills: Vec::new(),
        depends_on: Vec::new(),
    }];
    if matches_any(&["test", "verify", "validate", "check"]) {
        phases.push(ProposedPhase {
            name: "verify".to_owned(),
            objective: format!("Verify {summary}"),
            persona: String::new(),
            skills: Vec::new(),
            depends_on: vec!["implement".to_owned()],
        });
    }
    MissionProposal { phases }
}
