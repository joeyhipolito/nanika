use orchestrator_core::{
    AuthoredParseContext, ExecutionMode, MissionParseError, PersonaCatalog, PhaseId,
    PhasePolicyFixture, RuntimePolicyFixture, parse_authored_phases, validate_phase_ids,
};
use std::{collections::BTreeSet, path::PathBuf, time::Duration};

const DEFAULTS: &str = include_str!("../../../tests/fixtures/core-parity/mission-defaults.md");
const EXPLICIT_RUNTIME: &str =
    include_str!("../../../tests/fixtures/core-parity/mission-explicit-runtime.md");

fn context() -> AuthoredParseContext {
    AuthoredParseContext {
        user_home: Some(PathBuf::from("/fixture/home")),
        persona_catalog: PersonaCatalog {
            names: BTreeSet::from(["known-planner".to_owned()]),
        },
        target_context: None,
        policy: RuntimePolicyFixture {
            default: PhasePolicyFixture {
                fallback_persona: "fixture-implementer".to_owned(),
                fallback_selection_method: "fixture-policy".to_owned(),
                tier: "work".to_owned(),
                role: "implementer".to_owned(),
                runtime: "claude".to_owned(),
            },
            phases: Default::default(),
        },
    }
}

#[test]
fn authored_fixture_matches_go_dependency_visibility() -> Result<(), Box<dyn std::error::Error>> {
    let plan = parse_authored_phases(DEFAULTS, &context())?;

    assert_eq!(
        plan.phases.len(),
        3,
        "duplicate and malformed records are skipped"
    );
    assert_eq!(plan.execution_mode, ExecutionMode::Parallel);
    assert_eq!(plan.phases[0].skills, ["rust", "rust"]);
    assert_eq!(
        plan.phases[0].dependencies,
        [PhaseId::new("phase-1")?],
        "Go makes the current phase name visible while parsing dependencies"
    );
    assert_eq!(plan.phases[0].persona, "known-planner");
    assert_eq!(plan.phases[0].persona_selection_method, "llm");
    assert_eq!(plan.phases[1].persona, "fixture-implementer");
    assert_eq!(plan.phases[1].dependencies, [PhaseId::new("phase-1")?]);
    assert_eq!(plan.phases[1].workdir, "/fixture/home/repo");
    assert_eq!(plan.phases[1].stall_timeout, Some(Duration::from_secs(90)));
    assert_eq!(plan.phases[1].priority, "P0");
    assert_eq!(plan.phases[2].dependencies, [PhaseId::new("phase-2")?]);
    assert!(plan.phases.iter().all(|phase| phase.status == "pending"));
    Ok(())
}

#[test]
fn explicit_runtime_is_preserved_and_invalid_runtime_uses_injected_policy()
-> Result<(), Box<dyn std::error::Error>> {
    let plan = parse_authored_phases(EXPLICIT_RUNTIME, &context())?;

    assert_eq!(plan.phases[0].runtime, "codex");
    assert!(!plan.phases[0].runtime_policy_applied);
    assert_eq!(plan.phases[1].runtime, "claude");
    assert!(plan.phases[1].runtime_policy_applied);
    assert_eq!(plan.phases[1].stall_timeout, None);
    Ok(())
}

#[test]
fn authored_timeout_accepts_go_compatible_leading_plus() -> Result<(), Box<dyn std::error::Error>> {
    let plan = parse_authored_phases(
        "PHASE: inspect | OBJECTIVE: inspect | TIMEOUT: +1m30s",
        &context(),
    )?;

    assert_eq!(plan.phases[0].stall_timeout, Some(Duration::from_secs(90)));
    Ok(())
}

#[test]
fn execution_mode_derivation_is_table_driven() -> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        ("PHASE: one | OBJECTIVE: one", ExecutionMode::Sequential),
        (
            "PHASE: one | OBJECTIVE: one\nPHASE: two | OBJECTIVE: two",
            ExecutionMode::Parallel,
        ),
        (
            "PHASE: root | OBJECTIVE: root\nPHASE: left | OBJECTIVE: left | DEPENDS: root\nPHASE: right | OBJECTIVE: right | DEPENDS: root",
            ExecutionMode::Parallel,
        ),
        (
            "PHASE: one | OBJECTIVE: one\nPHASE: two | OBJECTIVE: two | DEPENDS: one\nPHASE: three | OBJECTIVE: three | DEPENDS: two",
            ExecutionMode::Sequential,
        ),
    ];
    for (source, expected) in cases {
        assert_eq!(
            parse_authored_phases(source, &context())?.execution_mode,
            expected
        );
    }
    Ok(())
}

#[test]
fn duplicate_phase_ids_are_rejected_before_runtime_use() -> Result<(), Box<dyn std::error::Error>> {
    let mut phases = parse_authored_phases(DEFAULTS, &context())?.phases;
    phases[1].id = phases[0].id.clone();
    assert_eq!(
        validate_phase_ids(&phases),
        Err(MissionParseError::DuplicatePhaseId("phase-1".to_owned()))
    );
    Ok(())
}

#[test]
fn malformed_and_unknown_fields_never_create_a_phase() {
    let malformed = [
        "",
        "phase: lower | OBJECTIVE: no",
        "PHASE: name",
        "PHASE: | OBJECTIVE: no name",
        "prefix PHASE: name | OBJECTIVE: no",
        "UNKNOWN: x | OBJECTIVE: no",
    ];
    for source in malformed {
        assert_eq!(
            parse_authored_phases(source, &context()),
            Err(MissionParseError::NoPhases)
        );
    }
}

#[test]
fn last_duplicate_field_wins_and_accepted_phases_are_capped_at_twelve()
-> Result<(), Box<dyn std::error::Error>> {
    let mut source =
        String::from("PHASE: first | OBJECTIVE: stale | OBJECTIVE: current | UNKNOWN: ignored\n");
    for index in 2..=13 {
        source.push_str(&format!("PHASE: p{index} | OBJECTIVE: phase {index}\n"));
    }
    let plan = parse_authored_phases(&source, &context())?;
    assert_eq!(plan.phases.len(), 12);
    assert_eq!(plan.phases[0].objective, "current");
    assert_eq!(plan.phases[11].name, "p12");
    Ok(())
}
