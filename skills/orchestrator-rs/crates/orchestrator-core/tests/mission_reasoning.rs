use orchestrator_core::{
    AuthoredParseContext, CompiledPlan, ExecutionMode, MAX_PROPOSAL_BYTES, MissionProposal,
    PersonaCatalog, PhaseId, PhasePolicyFixture, ProposalError, ProposedPhase,
    RuntimePolicyFixture, compile_proposal, keyword_fallback_proposal,
};
use std::{collections::BTreeSet, path::PathBuf};

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

fn proposed(name: &str, depends_on: &[&str]) -> ProposedPhase {
    ProposedPhase {
        name: name.to_owned(),
        objective: format!("objective for {name}"),
        persona: String::new(),
        skills: Vec::new(),
        depends_on: depends_on.iter().map(|value| (*value).to_owned()).collect(),
    }
}

#[test]
fn valid_multi_phase_proposal_compiles_into_a_resolved_dag()
-> Result<(), Box<dyn std::error::Error>> {
    let proposal = MissionProposal {
        phases: vec![
            proposed("root", &[]),
            proposed("left", &["root"]),
            proposed("right", &["root"]),
        ],
    };

    let CompiledPlan {
        phases,
        execution_mode,
    } = compile_proposal(&proposal, &context())?;

    assert_eq!(phases.len(), 3);
    assert_eq!(phases[0].id, PhaseId::new("phase-1")?);
    assert_eq!(phases[1].id, PhaseId::new("phase-2")?);
    assert_eq!(phases[2].id, PhaseId::new("phase-3")?);
    assert_eq!(phases[0].name, "root");
    assert!(phases[0].dependencies.is_empty());
    assert_eq!(phases[1].dependencies, [PhaseId::new("phase-1")?]);
    assert_eq!(phases[2].dependencies, [PhaseId::new("phase-1")?]);
    // Two phases sharing the same dependency set fan out in parallel.
    assert_eq!(execution_mode, ExecutionMode::Parallel);
    // A proposal with no RUNTIME override resolves the policy runtime, exactly
    // like an authored plan.
    assert!(phases.iter().all(|phase| phase.runtime == "claude"));
    assert!(phases.iter().all(|phase| phase.status == "pending"));
    Ok(())
}

#[test]
fn sequential_chain_compiles_as_sequential() -> Result<(), Box<dyn std::error::Error>> {
    let proposal = MissionProposal {
        phases: vec![
            proposed("root", &[]),
            proposed("mid", &["root"]),
            proposed("leaf", &["mid"]),
        ],
    };

    let plan = compile_proposal(&proposal, &context())?;

    assert_eq!(plan.execution_mode, ExecutionMode::Sequential);
    assert_eq!(plan.phases[2].dependencies, [PhaseId::new("phase-2")?]);
    Ok(())
}

#[test]
fn known_persona_wins_and_unknown_persona_falls_back_to_policy()
-> Result<(), Box<dyn std::error::Error>> {
    let proposal = MissionProposal {
        phases: vec![
            ProposedPhase {
                name: "plan".to_owned(),
                objective: "plan the work".to_owned(),
                persona: "known-planner".to_owned(),
                skills: vec!["rust".to_owned(), String::new(), " go ".to_owned()],
                depends_on: Vec::new(),
            },
            ProposedPhase {
                name: "build".to_owned(),
                objective: "build the work".to_owned(),
                persona: "not-in-catalog".to_owned(),
                skills: Vec::new(),
                depends_on: vec!["plan".to_owned()],
            },
        ],
    };

    let plan = compile_proposal(&proposal, &context())?;

    assert_eq!(plan.phases[0].persona, "known-planner");
    assert_eq!(plan.phases[0].persona_selection_method, "llm");
    // Blank skills are dropped and surviving skills are trimmed.
    assert_eq!(plan.phases[0].skills, ["rust", "go"]);
    assert_eq!(plan.phases[1].persona, "fixture-implementer");
    assert_eq!(plan.phases[1].persona_selection_method, "fixture-policy");
    assert_eq!(plan.phases[0].model_tier, "work");
    assert_eq!(plan.phases[0].role, "implementer");
    Ok(())
}

#[test]
fn duplicate_dependency_is_deduplicated_not_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let proposal = MissionProposal {
        phases: vec![proposed("root", &[]), proposed("leaf", &["root", "root"])],
    };

    let plan = compile_proposal(&proposal, &context())?;

    assert_eq!(plan.phases[1].dependencies, [PhaseId::new("phase-1")?]);
    Ok(())
}

#[test]
fn empty_proposal_is_rejected_and_produces_no_plan() {
    let proposal = MissionProposal { phases: Vec::new() };

    assert_eq!(
        compile_proposal(&proposal, &context()),
        Err(ProposalError::Empty)
    );
}

#[test]
fn proposal_over_the_phase_limit_is_rejected() {
    let phases = (1..=13)
        .map(|index| proposed(&format!("p{index}"), &[]))
        .collect();
    let proposal = MissionProposal { phases };

    assert_eq!(
        compile_proposal(&proposal, &context()),
        Err(ProposalError::TooManyPhases { max: 12 })
    );
}

#[test]
fn exactly_twelve_phases_is_accepted() -> Result<(), Box<dyn std::error::Error>> {
    let phases = (1..=12)
        .map(|index| proposed(&format!("p{index}"), &[]))
        .collect();
    let proposal = MissionProposal { phases };

    let plan = compile_proposal(&proposal, &context())?;

    assert_eq!(plan.phases.len(), 12);
    Ok(())
}

#[test]
fn missing_name_is_rejected_with_its_index() {
    let proposal = MissionProposal {
        phases: vec![ProposedPhase {
            name: "   ".to_owned(),
            objective: "has an objective".to_owned(),
            persona: String::new(),
            skills: Vec::new(),
            depends_on: Vec::new(),
        }],
    };

    assert_eq!(
        compile_proposal(&proposal, &context()),
        Err(ProposalError::MissingField {
            index: 0,
            field: "name",
        })
    );
}

#[test]
fn missing_objective_is_rejected_with_its_index() {
    let proposal = MissionProposal {
        phases: vec![
            proposed("first", &[]),
            ProposedPhase {
                name: "second".to_owned(),
                objective: String::new(),
                persona: String::new(),
                skills: Vec::new(),
                depends_on: Vec::new(),
            },
        ],
    };

    assert_eq!(
        compile_proposal(&proposal, &context()),
        Err(ProposalError::MissingField {
            index: 1,
            field: "objective",
        })
    );
}

#[test]
fn duplicate_phase_name_is_rejected() {
    let proposal = MissionProposal {
        phases: vec![proposed("build", &[]), proposed("build", &[])],
    };

    assert_eq!(
        compile_proposal(&proposal, &context()),
        Err(ProposalError::DuplicateName {
            name: "build".to_owned(),
        })
    );
}

#[test]
fn unknown_dependency_is_rejected() {
    let proposal = MissionProposal {
        phases: vec![proposed("build", &["ghost"])],
    };

    assert_eq!(
        compile_proposal(&proposal, &context()),
        Err(ProposalError::UnknownDependency {
            name: "build".to_owned(),
            dependency: "ghost".to_owned(),
        })
    );
}

#[test]
fn self_dependency_is_rejected_distinctly_from_a_cycle() {
    let proposal = MissionProposal {
        phases: vec![proposed("build", &["build"])],
    };

    assert_eq!(
        compile_proposal(&proposal, &context()),
        Err(ProposalError::SelfDependency {
            name: "build".to_owned(),
        })
    );
}

#[test]
fn dependency_cycle_is_rejected_via_the_reducer_detector() {
    // a -> b and b -> a: resolvable names, so the reducer's cycle detector is
    // what rejects it — not the unknown-dependency pre-check.
    let proposal = MissionProposal {
        phases: vec![proposed("a", &["b"]), proposed("b", &["a"])],
    };

    assert_eq!(
        compile_proposal(&proposal, &context()),
        Err(ProposalError::Cycle {
            name: "a".to_owned(),
        })
    );
}

#[test]
fn malformed_json_is_rejected() {
    assert_eq!(
        MissionProposal::from_json_bytes(b"{ not valid json"),
        Err(ProposalError::Malformed)
    );
}

#[test]
fn oversize_json_is_rejected_before_parsing() {
    let oversized = vec![b' '; MAX_PROPOSAL_BYTES + 1];

    assert_eq!(
        MissionProposal::from_json_bytes(&oversized),
        Err(ProposalError::Malformed)
    );
}

#[test]
fn valid_json_proposal_parses_and_compiles() -> Result<(), Box<dyn std::error::Error>> {
    let capsule = br#"{"phases":[
        {"name":"build","objective":"do the work"},
        {"name":"verify","objective":"check the work","depends_on":["build"]}
    ]}"#;

    let proposal = MissionProposal::from_json_bytes(capsule)?;
    let plan = compile_proposal(&proposal, &context())?;

    assert_eq!(plan.phases.len(), 2);
    assert_eq!(plan.phases[1].dependencies, [PhaseId::new("phase-1")?]);
    // An unspecified persona resolves to the policy fallback.
    assert_eq!(plan.phases[0].persona, "fixture-implementer");
    Ok(())
}

#[test]
fn keyword_fallback_is_deterministic() {
    let task = "Please add tests and verify the parser";

    let first = keyword_fallback_proposal(task);
    let second = keyword_fallback_proposal(task);

    assert_eq!(first, second);
}

#[test]
fn keyword_fallback_implement_verify_task_compiles_with_a_dependency()
-> Result<(), Box<dyn std::error::Error>> {
    let proposal = keyword_fallback_proposal("Implement the feature and add tests");

    assert_eq!(proposal.phases.len(), 2);
    assert_eq!(proposal.phases[0].name, "implement");
    assert_eq!(proposal.phases[1].name, "verify");
    assert_eq!(proposal.phases[1].depends_on, ["implement"]);

    let plan = compile_proposal(&proposal, &context())?;
    assert_eq!(plan.phases[1].dependencies, [PhaseId::new("phase-1")?]);
    Ok(())
}

#[test]
fn keyword_fallback_research_task_has_a_single_phase() -> Result<(), Box<dyn std::error::Error>> {
    let proposal = keyword_fallback_proposal("Research the competitive landscape");

    assert_eq!(proposal.phases.len(), 1);
    assert_eq!(proposal.phases[0].name, "research");

    let plan = compile_proposal(&proposal, &context())?;
    assert_eq!(plan.execution_mode, ExecutionMode::Sequential);
    Ok(())
}

#[test]
fn keyword_fallback_plain_task_is_a_single_implement_phase()
-> Result<(), Box<dyn std::error::Error>> {
    let proposal = keyword_fallback_proposal("Ship the landing page");

    assert_eq!(proposal.phases.len(), 1);
    assert_eq!(proposal.phases[0].name, "implement");

    // Even an empty task yields a compilable plan.
    let empty = keyword_fallback_proposal("   ");
    assert!(compile_proposal(&empty, &context()).is_ok());
    Ok(())
}
