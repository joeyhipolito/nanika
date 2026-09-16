//! Strict authored-PHASE execution for the experimental local pilot.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use orchestrator_core::{
    AuthoredParseContext, AuthoredPhase, MissionProposal, PersonaCatalog, PhasePolicyFixture,
    ProposedPhase, RuntimePolicyFixture, compile_proposal,
};
use orchestrator_exec::AttemptOutcome;
use orchestrator_process::{CancellationToken, ProcessSupervisor};
use serde_json::{Value, json};

use super::*;

const PLAN_SCHEMA: &str = "nanika.rust-first-use-authored-plan.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PhaseRole {
    Code,
    Review,
    Verification,
}

impl PhaseRole {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Review => "review",
            Self::Verification => "verification",
        }
    }

    const fn request_role(self) -> &'static str {
        match self {
            Self::Code => "implementer",
            Self::Review => "reviewer",
            Self::Verification => "verifier",
        }
    }
}

#[derive(Debug)]
pub(crate) struct AuthoredMission {
    pub(crate) phases: Vec<AuthoredPhase>,
    pub(crate) roles: Vec<PhaseRole>,
    pub(crate) execution_order: Vec<usize>,
}

pub(crate) enum PhaseAction {
    Execute(usize),
    Skip {
        index: usize,
        failed_dependencies: Vec<String>,
    },
    Complete,
}

pub(crate) struct PhaseEnvironment<'a> {
    pub(crate) options: &'a PilotOptions,
    pub(crate) snapshot: &'a snapshot::RepositorySnapshot,
    pub(crate) observed_version: &'a str,
    pub(crate) cancellation: &'a CancellationToken,
    pub(crate) supervisor: &'a ProcessSupervisor,
    pub(crate) shell_config: Option<PathBuf>,
    pub(crate) durable_processes: Option<&'a durable::DurableProcessOwner>,
}

pub(crate) struct PhaseFailure {
    pub(crate) reason: String,
    pub(crate) record: Option<Value>,
    stage: FailureStage,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FailureStage {
    Preparation,
    ExecutionEntered,
}

impl PhaseFailure {
    fn before_dispatch(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            record: None,
            stage: FailureStage::Preparation,
        }
    }

    // Entering an execution helper is conservatively treated as an attempt:
    // its errors may occur after the child ran, even without a result record.
    fn execution_entered(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            record: None,
            stage: FailureStage::ExecutionEntered,
        }
    }

    pub(crate) fn is_preparation_refusal(&self) -> bool {
        self.stage == FailureStage::Preparation
    }
}

struct DispatchedPhase<'a> {
    phase: &'a AuthoredPhase,
    role: PhaseRole,
    route: &'a routing::Route,
    observed_version: &'a str,
    outcome: &'a AttemptOutcome,
    observation: Option<&'a Observation>,
}

impl DispatchedPhase<'_> {
    fn record(&self, passed: bool, reason: &str) -> Value {
        let mut record = cycle::attempt_record(
            &self.phase.name,
            passed,
            reason,
            self.route,
            self.observed_version,
            self.outcome,
            self.observation,
        );
        decorate(&mut record, self.phase, self.role, true);
        record
    }

    fn failure(&self, reason: impl Into<String>) -> PhaseFailure {
        let reason = reason.into();
        let mut record = self.record(false, &reason);
        if self.role == PhaseRole::Review {
            record["verdict_valid"] = json!(false);
        }
        PhaseFailure {
            reason,
            record: Some(record),
            stage: FailureStage::ExecutionEntered,
        }
    }
}

pub(crate) fn run(
    options: &PilotOptions,
    cancellation: CancellationToken,
) -> Result<PilotSummary, PilotError> {
    let source = read_prompt_with_limit(&options.prompt_file, MAX_CODE_PROMPT_BYTES)?;
    let mission = parse_mission(&source).map_err(PilotError::Composition)?;
    let layout = create_output_layout(&options.output_dir, PilotCommand::Run)?;
    write_artifact(&layout.root.join("mission.md"), source.as_bytes())?;
    write_plan(&layout.root, &mission)?;

    let mut results = BTreeMap::new();
    let outcome = execute(options, &mission, &layout, &cancellation, &mut results);
    if let Err(reason) = &outcome {
        record_unfinished(&layout.root, &mission, &mut results, reason)?;
    }
    terminal(
        &layout.root,
        &mission,
        results,
        outcome.as_ref().err().map(String::as_str),
    )
}

pub(crate) fn parse_mission(source: &str) -> Result<AuthoredMission, String> {
    let mut proposed = Vec::new();
    let mut roles = Vec::new();
    for (line_index, raw_line) in source.lines().enumerate() {
        let line = raw_line.trim().trim_matches('`').trim();
        if !line.starts_with("PHASE:") {
            continue;
        }
        let line_number = line_index + 1;
        let mut fields = BTreeMap::new();
        for part in line.split('|') {
            let (raw_key, raw_value) = part.split_once(':').ok_or_else(|| {
                format!("line {line_number} contains a PHASE field without a colon")
            })?;
            let key = raw_key.trim();
            let value = raw_value.trim();
            if !matches!(key, "PHASE" | "OBJECTIVE" | "PERSONA" | "DEPENDS" | "ROLE") {
                return Err(format!(
                    "line {line_number} uses unsupported authored field {key:?}"
                ));
            }
            if fields.insert(key, value).is_some() {
                return Err(format!("line {line_number} repeats authored field {key:?}"));
            }
        }
        let required = |key: &'static str| {
            fields
                .get(key)
                .copied()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("line {line_number} is missing required field {key}"))
        };
        let name = required("PHASE")?;
        validate_name(name, line_number)?;
        let objective = required("OBJECTIVE")?;
        let persona = required("PERSONA")?;
        let role = resolve_role(
            fields.get("ROLE").copied(),
            name,
            persona,
            objective,
            line_number,
        )?;
        let mut seen_dependencies = BTreeSet::new();
        let mut depends_on = Vec::new();
        if let Some(authored) = fields.get("DEPENDS").filter(|value| !value.is_empty()) {
            for dependency in authored.split(',').map(str::trim) {
                if dependency.is_empty() {
                    return Err(format!(
                        "line {line_number} contains an empty dependency name"
                    ));
                }
                validate_name(dependency, line_number)?;
                if !seen_dependencies.insert(dependency) {
                    return Err(format!(
                        "line {line_number} repeats dependency {dependency:?}"
                    ));
                }
                depends_on.push(dependency.to_owned());
            }
        }
        proposed.push(ProposedPhase {
            name: name.to_owned(),
            objective: objective.to_owned(),
            persona: persona.to_owned(),
            skills: Vec::new(),
            depends_on,
        });
        roles.push(role);
    }
    if proposed.is_empty() {
        return Err("authored mission contains no PHASE records".to_owned());
    }

    require_role(&roles, PhaseRole::Code)?;
    require_role(&roles, PhaseRole::Review)?;
    require_role(&roles, PhaseRole::Verification)?;

    let mut persona_catalog = PersonaCatalog::default();
    let mut policy = RuntimePolicyFixture::default();
    for (phase, role) in proposed.iter().zip(&roles) {
        persona_catalog.names.insert(phase.persona.clone());
        policy.phases.insert(
            phase.name.clone(),
            PhasePolicyFixture {
                fallback_persona: phase.persona.clone(),
                fallback_selection_method: "authored-pilot".to_owned(),
                tier: "work".to_owned(),
                role: role.request_role().to_owned(),
                runtime: "codex".to_owned(),
            },
        );
    }
    let context = AuthoredParseContext {
        persona_catalog,
        policy,
        ..AuthoredParseContext::default()
    };
    let compiled = compile_proposal(&MissionProposal { phases: proposed }, &context)
        .map_err(|error| format!("authored mission rejected: {error}"))?;
    let execution_order = dependency_order(&compiled.phases)?;
    validate_role_dependencies(&compiled.phases, &roles)?;
    Ok(AuthoredMission {
        phases: compiled.phases,
        roles,
        execution_order,
    })
}

#[cfg(test)]
pub(crate) fn validate_mission(source: &str) -> Result<(), String> {
    parse_mission(source).map(|_| ())
}

fn validate_name(name: &str, line_number: usize) -> Result<(), String> {
    let valid = name.len() <= 64
        && name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "line {line_number} phase and dependency names must be 1..64 ASCII letters, digits, '-' or '_', starting with a letter or digit"
        ))
    }
}

fn resolve_role(
    authored: Option<&str>,
    name: &str,
    persona: &str,
    objective: &str,
    line_number: usize,
) -> Result<PhaseRole, String> {
    if let Some(role) = authored {
        return match role.trim().to_ascii_lowercase().as_str() {
            "code" | "coding" | "implementer" => Ok(PhaseRole::Code),
            "review" | "reviewer" => Ok(PhaseRole::Review),
            "verification" | "verifier" => Ok(PhaseRole::Verification),
            role => Err(format!(
                "line {line_number} uses unsupported role {role:?}; expected code, review, or verification"
            )),
        };
    }

    let normalized_name = name.to_ascii_lowercase();
    let normalized_persona = persona.to_ascii_lowercase();
    if normalized_persona == "operator-verifier"
        || contains_token(&normalized_name, &["verify", "verification"])
    {
        return Ok(PhaseRole::Verification);
    }
    if matches!(
        normalized_persona.as_str(),
        "reviewer" | "staff-code-reviewer" | "security-auditor" | "academic-reviewer"
    ) || contains_token(&normalized_name, &["review", "audit"])
    {
        return Ok(PhaseRole::Review);
    }
    let lowered_objective = objective.to_ascii_lowercase();
    let unsupported_planner = matches!(
        normalized_persona.as_str(),
        "architect"
            | "system-architect"
            | "solutions-architect"
            | "data-analyst"
            | "academic-researcher"
            | "methodologist"
    ) || contains_token(
        &normalized_name,
        &[
            "plan",
            "planning",
            "design",
            "research",
            "investigate",
            "analysis",
        ],
    ) || [
        "design the",
        "plan the",
        "research how",
        "investigate the",
        "create a plan",
    ]
    .iter()
    .any(|needle| lowered_objective.contains(needle));
    if unsupported_planner {
        Err(format!(
            "line {line_number} resolves to unsupported planner work; authored pilot roles are code, review, and verification"
        ))
    } else {
        Ok(PhaseRole::Code)
    }
}

fn contains_token(value: &str, candidates: &[&str]) -> bool {
    value
        .split(['-', '_'])
        .any(|token| candidates.contains(&token))
}

fn require_role(roles: &[PhaseRole], required: PhaseRole) -> Result<(), String> {
    if roles.contains(&required) {
        Ok(())
    } else {
        Err(format!(
            "authored mission requires at least one {} phase",
            required.as_str()
        ))
    }
}

fn dependency_order(phases: &[AuthoredPhase]) -> Result<Vec<usize>, String> {
    let mut scheduled = BTreeSet::new();
    let mut order = Vec::with_capacity(phases.len());
    while order.len() < phases.len() {
        let Some((index, phase)) = phases.iter().enumerate().find(|(_, phase)| {
            !scheduled.contains(&phase.id)
                && phase
                    .dependencies
                    .iter()
                    .all(|dependency| scheduled.contains(dependency))
        }) else {
            return Err("compiled mission did not yield a dependency order".to_owned());
        };
        scheduled.insert(phase.id.clone());
        order.push(index);
    }
    Ok(order)
}

fn validate_role_dependencies(phases: &[AuthoredPhase], roles: &[PhaseRole]) -> Result<(), String> {
    for (index, role) in roles.iter().enumerate() {
        match role {
            PhaseRole::Code => {
                require_descendant_role(index, PhaseRole::Review, phases, roles)?;
                require_descendant_role(index, PhaseRole::Verification, phases, roles)?;
            }
            PhaseRole::Review => {
                if !has_ancestor_role(index, PhaseRole::Code, phases, roles) {
                    return Err(format!(
                        "review phase {:?} must depend directly or transitively on a code phase",
                        phases[index].name
                    ));
                }
                require_descendant_role(index, PhaseRole::Verification, phases, roles)?;
            }
            PhaseRole::Verification => {
                if !has_ancestor_role(index, PhaseRole::Review, phases, roles) {
                    return Err(format!(
                        "verification phase {:?} must depend directly or transitively on a review phase",
                        phases[index].name
                    ));
                }
            }
        }
    }
    Ok(())
}

fn require_descendant_role(
    index: usize,
    required: PhaseRole,
    phases: &[AuthoredPhase],
    roles: &[PhaseRole],
) -> Result<(), String> {
    if phases.iter().enumerate().any(|(candidate, _)| {
        roles[candidate] == required && has_ancestor_index(candidate, index, phases)
    }) {
        Ok(())
    } else {
        Err(format!(
            "{} phase {:?} must be covered by a later {} phase",
            roles[index].as_str(),
            phases[index].name,
            required.as_str()
        ))
    }
}

fn has_ancestor_role(
    index: usize,
    required: PhaseRole,
    phases: &[AuthoredPhase],
    roles: &[PhaseRole],
) -> bool {
    let id_to_index: BTreeMap<_, _> = phases
        .iter()
        .enumerate()
        .map(|(position, phase)| (phase.id.clone(), position))
        .collect();
    let mut pending = phases[index].dependencies.clone();
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let Some(ancestor) = id_to_index.get(&id).copied() else {
            return false;
        };
        if roles[ancestor] == required {
            return true;
        }
        pending.extend(phases[ancestor].dependencies.iter().cloned());
    }
    false
}

fn has_ancestor_index(index: usize, required: usize, phases: &[AuthoredPhase]) -> bool {
    ancestor_indices(index, phases).contains(&required)
}

fn write_plan(root: &Path, mission: &AuthoredMission) -> Result<(), PilotError> {
    let names: BTreeMap<_, _> = mission
        .phases
        .iter()
        .map(|phase| (phase.id.clone(), phase.name.as_str()))
        .collect();
    let phases: Vec<Value> = mission
        .phases
        .iter()
        .zip(&mission.roles)
        .enumerate()
        .map(|(index, (phase, role))| {
            json!({
                "id": phase.id.as_str(),
                "name": phase.name,
                "objective": phase.objective,
                "persona": phase.persona,
                "role": role.as_str(),
                "dependencies": phase.dependencies.iter().filter_map(|id| names.get(id)).collect::<Vec<_>>(),
                "artifact_directory": phase_directory(root, index, phase),
            })
        })
        .collect();
    let execution_order: Vec<&str> = mission
        .execution_order
        .iter()
        .map(|index| mission.phases[*index].name.as_str())
        .collect();
    let record = json!({
        "schema": PLAN_SCHEMA,
        "execution": "sequential_dependency_order",
        "execution_order": execution_order,
        "phases": phases,
    });
    let mut bytes = serde_json::to_vec_pretty(&record).map_err(|error| PilotError::Artifact {
        path: root.join("plan.json"),
        source: io::Error::other(error),
    })?;
    bytes.push(b'\n');
    write_artifact(&root.join("plan.json"), &bytes)
}

fn execute(
    options: &PilotOptions,
    mission: &AuthoredMission,
    layout: &OutputLayout,
    cancellation: &CancellationToken,
    results: &mut BTreeMap<usize, Value>,
) -> Result<(), String> {
    let supervisor = ProcessSupervisor::process_wide().map_err(|error| error.to_string())?;
    let repo = options
        .repo
        .as_deref()
        .ok_or_else(|| "authored run lost its required repository".to_owned())?;
    let snapshot = snapshot::prepare(repo, &layout.root, &supervisor, cancellation)?;
    let phases_root = layout.root.join("phases");
    cycle::private_dir(&phases_root)?;
    for (index, phase) in mission.phases.iter().enumerate() {
        cycle::private_dir(&phase_directory(&layout.root, index, phase))?;
    }
    let observed_version =
        probe_runtime_version(options, &snapshot.workspace, cancellation, &supervisor)
            .map_err(|error| error.to_string())?;
    let environment = PhaseEnvironment {
        options,
        snapshot: &snapshot,
        observed_version: &observed_version,
        cancellation,
        supervisor: &supervisor,
        shell_config: layout.shell_config.clone(),
        durable_processes: None,
    };

    loop {
        let action = next_phase_action(mission, results);
        let (index, failed_dependencies) = match action {
            PhaseAction::Execute(index) => (index, Vec::new()),
            PhaseAction::Skip {
                index,
                failed_dependencies,
            } => (index, failed_dependencies),
            PhaseAction::Complete => break,
        };
        let phase = &mission.phases[index];
        let phase_root = phase_directory(&layout.root, index, phase);
        if !failed_dependencies.is_empty() {
            let record = json!({
                "phase": phase.name,
                "id": phase.id.as_str(),
                "role": mission.roles[index].as_str(),
                "persona": phase.persona,
                "objective": phase.objective,
                "status": "skipped",
                "reason": "one or more dependencies did not pass",
                "skipped_dependencies": failed_dependencies,
                "provider_dispatched": false,
            });
            write_phase_record(&phase_root, &record)?;
            results.insert(index, record);
            continue;
        }

        let result = match mission.roles[index] {
            PhaseRole::Code => run_code_phase(phase, &phase_root, &environment),
            PhaseRole::Review => run_review_phase(mission, index, &phase_root, &environment),
            PhaseRole::Verification => run_verification_phase(phase, &phase_root, &environment),
        };
        let record = match result {
            Ok(record) => record,
            Err(failure) => failure.record.unwrap_or_else(|| {
                json!({
                    "phase": phase.name,
                    "id": phase.id.as_str(),
                    "role": mission.roles[index].as_str(),
                    "persona": phase.persona,
                    "objective": phase.objective,
                    "status": "failed",
                    "reason": failure.reason,
                    "skipped_dependencies": [],
                    "provider_dispatched": false,
                })
            }),
        };
        write_phase_record(&phase_root, &record)?;
        results.insert(index, record);
    }

    let diff = snapshot.diff(&supervisor)?;
    write_artifact(&layout.root.join("changes.diff"), &diff).map_err(|error| error.to_string())?;
    snapshot.verify_source(&supervisor)?;
    if results.values().any(|record| record["status"] != "passed") {
        Err("one or more authored phases did not pass".to_owned())
    } else {
        Ok(())
    }
}

pub(crate) fn next_phase_action(
    mission: &AuthoredMission,
    results: &BTreeMap<usize, Value>,
) -> PhaseAction {
    let Some(index) = mission
        .execution_order
        .iter()
        .copied()
        .find(|index| !results.contains_key(index))
    else {
        return PhaseAction::Complete;
    };
    let failed_dependencies = dependency_names(&mission.phases[index], &mission.phases)
        .into_iter()
        .filter(|(_, dependency_index)| {
            !results
                .get(dependency_index)
                .is_some_and(|record| record["status"] == "passed")
        })
        .map(|(name, _)| name.to_owned())
        .collect::<Vec<_>>();
    if failed_dependencies.is_empty() {
        PhaseAction::Execute(index)
    } else {
        PhaseAction::Skip {
            index,
            failed_dependencies,
        }
    }
}

pub(crate) fn run_code_phase(
    phase: &AuthoredPhase,
    phase_root: &Path,
    environment: &PhaseEnvironment<'_>,
) -> Result<Value, PhaseFailure> {
    let mut phase_options = environment.options.clone();
    phase_options.command = PilotCommand::Code;
    phase_options.persona = Some(phase.persona.clone());
    let route = routing::select(&phase_options, &phase.objective);
    let service = provider_service(phase, PhaseRole::Code, environment)
        .map_err(PhaseFailure::before_dispatch)?;
    let outcome = dispatch_phase(
        &phase_options,
        &phase.objective,
        &route,
        &environment.snapshot.workspace,
        service.as_ref(),
        phase.id.as_str(),
        PhaseRole::Code.request_role(),
    )
    .map_err(|error| PhaseFailure::execution_entered(error.to_string()))?;
    let observation = service.take_observation();
    let dispatched = DispatchedPhase {
        phase,
        role: PhaseRole::Code,
        route: &route,
        observed_version: environment.observed_version,
        outcome: &outcome,
        observation: observation.as_ref(),
    };
    cycle::save_observation(phase_root, observation.as_ref())
        .map_err(|reason| dispatched.failure(reason))?;
    if let Some(answer) = outcome.output() {
        write_artifact(&phase_root.join("answer.md"), answer.as_bytes())
            .map_err(|error| dispatched.failure(error.to_string()))?;
    }
    let diff = environment
        .snapshot
        .diff(environment.supervisor)
        .map_err(|reason| dispatched.failure(reason))?;
    write_artifact(&phase_root.join("changes.diff"), &diff)
        .map_err(|error| dispatched.failure(error.to_string()))?;
    environment
        .snapshot
        .verify_source(environment.supervisor)
        .map_err(|reason| dispatched.failure(reason))?;
    let passed = matches!(outcome, AttemptOutcome::Completed(_));
    Ok(dispatched.record(
        passed,
        if passed {
            "Codex coding phase completed"
        } else {
            "Codex coding phase failed"
        },
    ))
}

pub(crate) fn run_review_phase(
    mission: &AuthoredMission,
    index: usize,
    phase_root: &Path,
    environment: &PhaseEnvironment<'_>,
) -> Result<Value, PhaseFailure> {
    let phase = &mission.phases[index];
    let diff = environment
        .snapshot
        .diff(environment.supervisor)
        .map_err(PhaseFailure::before_dispatch)?;
    let applicable = applicable_review_task(mission, index);
    let review_task = format!(
        "Review objective:\n{}\n\nApplicable authored coding tasks:\n{}",
        phase.objective, applicable
    );
    let prompt = cycle::build_review_prompt(environment.snapshot, &review_task, &diff)
        .map_err(PhaseFailure::before_dispatch)?;
    write_artifact(&phase_root.join("prompt.md"), prompt.as_bytes())
        .map_err(|error| PhaseFailure::before_dispatch(error.to_string()))?;
    let mut phase_options = environment.options.clone();
    phase_options.command = PilotCommand::Review;
    phase_options.persona = Some(phase.persona.clone());
    let route = routing::select(&phase_options, &phase.objective);
    let service = provider_service(phase, PhaseRole::Review, environment)
        .map_err(PhaseFailure::before_dispatch)?;
    let outcome = dispatch_phase(
        &phase_options,
        &prompt,
        &route,
        &environment.snapshot.workspace,
        service.as_ref(),
        phase.id.as_str(),
        PhaseRole::Review.request_role(),
    )
    .map_err(|error| PhaseFailure::execution_entered(error.to_string()))?;
    let observation = service.take_observation();
    let dispatched = DispatchedPhase {
        phase,
        role: PhaseRole::Review,
        route: &route,
        observed_version: environment.observed_version,
        outcome: &outcome,
        observation: observation.as_ref(),
    };
    cycle::save_observation(phase_root, observation.as_ref())
        .map_err(|reason| dispatched.failure(reason))?;
    if let Some(answer) = outcome.output() {
        write_artifact(&phase_root.join("answer.md"), answer.as_bytes())
            .map_err(|error| dispatched.failure(error.to_string()))?;
    }
    let (mut passed, mut reason, verdict_valid) = match cycle::completed_review_answer(&outcome) {
        Ok(answer) => match cycle::parse_verdict(answer) {
            Ok(verdict) => {
                let bytes = serde_json::to_vec_pretty(&verdict)
                    .map_err(|error| dispatched.failure(error.to_string()))?;
                write_artifact(&phase_root.join("verdict.json"), &bytes)
                    .map_err(|error| dispatched.failure(error.to_string()))?;
                match verdict.verdict {
                    cycle::Verdict::Pass if verdict.blockers.is_empty() => (
                        true,
                        "structured review passed with zero blockers".to_owned(),
                        true,
                    ),
                    cycle::Verdict::Pass => (
                        false,
                        "review claimed pass while reporting blockers".to_owned(),
                        true,
                    ),
                    cycle::Verdict::Reject => {
                        (false, "review rejected the implementation".to_owned(), true)
                    }
                }
            }
            Err(reason) => (false, reason, false),
        },
        Err(reason) => (false, reason, false),
    };
    match environment.snapshot.diff(environment.supervisor) {
        Ok(after) if after == diff => {}
        Ok(_) => {
            passed = false;
            reason = "read-only review changed the coding workspace".to_owned();
        }
        Err(error) => {
            passed = false;
            reason = error;
        }
    }
    if passed {
        if let Err(error) = environment.snapshot.verify_source(environment.supervisor) {
            passed = false;
            reason = error;
        }
    }
    let mut record = dispatched.record(passed, &reason);
    record["verdict_valid"] = json!(verdict_valid);
    Ok(record)
}

pub(crate) fn run_verification_phase(
    phase: &AuthoredPhase,
    phase_root: &Path,
    environment: &PhaseEnvironment<'_>,
) -> Result<Value, PhaseFailure> {
    let checkpoint = environment
        .snapshot
        .checkpoint_workspace(&phase_root.join("reviewed-workspace"))
        .map_err(PhaseFailure::before_dispatch)?;
    let mut result = if let Some(owner) = environment.durable_processes {
        cycle::run_verification_at_durable(
            environment.options,
            &environment.snapshot.workspace,
            phase_root,
            phase,
            environment.cancellation,
            owner,
        )
    } else {
        cycle::run_verification_at(
            environment.options,
            &environment.snapshot.workspace,
            phase_root,
            &phase.name,
            &phase.persona,
            environment.cancellation,
            environment.supervisor,
        )
    }
    .map_err(PhaseFailure::execution_entered)?;
    cycle::assess_verification_workspace(
        environment.snapshot,
        &checkpoint,
        phase_root,
        environment.supervisor,
        &mut result,
    );
    if let Err(reason) = environment.snapshot.verify_source(environment.supervisor) {
        result.passed = false;
        result.reason = reason.clone();
        result.record["status"] = json!("failed");
        result.record["reason"] = json!(reason);
        result.record["source_repository_preserved"] = json!(false);
    } else {
        result.record["source_repository_preserved"] = json!(true);
    }
    decorate(&mut result.record, phase, PhaseRole::Verification, true);
    Ok(result.record)
}

fn provider_service(
    phase: &AuthoredPhase,
    role: PhaseRole,
    environment: &PhaseEnvironment<'_>,
) -> Result<Box<dyn ObservedProcessService>, String> {
    if let Some(owner) = environment.durable_processes {
        let executable = Path::new(&environment.options.codex_executable);
        let shell_config = (role == PhaseRole::Code)
            .then(|| environment.shell_config.clone())
            .flatten();
        return owner
            .service(
                phase.id.as_str(),
                executable,
                PathBuf::from("workspace"),
                shell_config,
                environment.cancellation.clone(),
            )
            .map(|service| Box::new(service) as Box<dyn ObservedProcessService>);
    }
    cycle::codex_service(
        environment.options,
        environment.cancellation.clone(),
        (role == PhaseRole::Code)
            .then(|| environment.shell_config.clone())
            .flatten(),
    )
    .map(|service| Box::new(service) as Box<dyn ObservedProcessService>)
}

pub(crate) fn applicable_review_task(mission: &AuthoredMission, index: usize) -> String {
    let mut ancestors = ancestor_indices(index, &mission.phases);
    ancestors.sort_unstable();
    ancestors
        .into_iter()
        .filter(|ancestor| mission.roles[*ancestor] == PhaseRole::Code)
        .map(|ancestor| {
            let phase = &mission.phases[ancestor];
            format!("- {}: {}", phase.name, phase.objective)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn ancestor_indices(index: usize, phases: &[AuthoredPhase]) -> Vec<usize> {
    let id_to_index: BTreeMap<_, _> = phases
        .iter()
        .enumerate()
        .map(|(position, phase)| (phase.id.clone(), position))
        .collect();
    let mut pending = phases[index].dependencies.clone();
    let mut ancestors = BTreeSet::new();
    while let Some(id) = pending.pop() {
        let Some(ancestor) = id_to_index.get(&id).copied() else {
            continue;
        };
        if ancestors.insert(ancestor) {
            pending.extend(phases[ancestor].dependencies.iter().cloned());
        }
    }
    ancestors.into_iter().collect()
}

fn decorate(record: &mut Value, phase: &AuthoredPhase, role: PhaseRole, dispatched: bool) {
    record["id"] = json!(phase.id.as_str());
    record["phase"] = json!(phase.name);
    record["role"] = json!(role.as_str());
    record["persona"] = json!(phase.persona);
    record["objective"] = json!(phase.objective);
    record["provider_dispatched"] = json!(dispatched && role != PhaseRole::Verification);
}

pub(crate) fn dependency_names<'a>(
    phase: &AuthoredPhase,
    phases: &'a [AuthoredPhase],
) -> Vec<(&'a str, usize)> {
    phase
        .dependencies
        .iter()
        .filter_map(|id| {
            phases
                .iter()
                .enumerate()
                .find(|(_, candidate)| candidate.id == *id)
                .map(|(index, candidate)| (candidate.name.as_str(), index))
        })
        .collect()
}

pub(crate) fn phase_directory(root: &Path, index: usize, phase: &AuthoredPhase) -> PathBuf {
    root.join("phases")
        .join(format!("{:02}-{}", index + 1, phase.name))
}

fn write_phase_record(root: &Path, record: &Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    write_artifact(&root.join("phase-result.json"), &bytes).map_err(|error| error.to_string())
}

fn record_unfinished(
    root: &Path,
    mission: &AuthoredMission,
    results: &mut BTreeMap<usize, Value>,
    reason: &str,
) -> Result<(), PilotError> {
    for index in &mission.execution_order {
        if results.contains_key(index) {
            continue;
        }
        let phase = &mission.phases[*index];
        let record = json!({
            "phase": phase.name,
            "id": phase.id.as_str(),
            "role": mission.roles[*index].as_str(),
            "persona": phase.persona,
            "objective": phase.objective,
            "status": "skipped",
            "reason": format!("mission preparation or execution failed: {reason}"),
            "skipped_dependencies": dependency_names(phase, &mission.phases).into_iter().map(|(name, _)| name).collect::<Vec<_>>(),
            "provider_dispatched": false,
        });
        let directory = phase_directory(root, *index, phase);
        let phases_root = root.join("phases");
        if !phases_root.exists() {
            cycle::private_dir(&phases_root).map_err(PilotError::Composition)?;
        }
        if !directory.exists() {
            cycle::private_dir(&directory).map_err(PilotError::Composition)?;
        }
        write_phase_record(&directory, &record).map_err(PilotError::Composition)?;
        results.insert(*index, record);
    }
    Ok(())
}

fn terminal(
    root: &Path,
    mission: &AuthoredMission,
    mut results: BTreeMap<usize, Value>,
    execution_error: Option<&str>,
) -> Result<PilotSummary, PilotError> {
    let phases: Vec<Value> = mission
        .execution_order
        .iter()
        .filter_map(|index| results.remove(index))
        .collect();
    let completed = execution_error.is_none()
        && phases.len() == mission.phases.len()
        && phases.iter().all(|phase| phase["status"] == "passed");
    let provider_completed = phases
        .iter()
        .filter(|phase| phase["role"] != "verification")
        .all(|phase| phase["status"] == "passed");
    let tests_verified = phases
        .iter()
        .filter(|phase| phase["role"] == "verification")
        .all(|phase| phase["status"] == "passed");
    let reason = if completed {
        "completed".to_owned()
    } else {
        phases
            .iter()
            .find(|phase| phase["status"] == "failed")
            .and_then(|phase| phase["reason"].as_str())
            .or(execution_error)
            .unwrap_or("mission did not complete")
            .to_owned()
    };
    let record = json!({
        "schema": RESULT_SCHEMA,
        "command": "run",
        "input": "authored-mission",
        "status": if completed { "completed" } else { "failed" },
        "reason": reason,
        "phases": phases,
        "provider_completed": provider_completed,
        "tests_verified": tests_verified,
    });
    let result_path = write_result(root, &record)?;
    Ok(PilotSummary {
        completed,
        status: if completed { "completed" } else { "failed" },
        reason,
        result_path,
    })
}
