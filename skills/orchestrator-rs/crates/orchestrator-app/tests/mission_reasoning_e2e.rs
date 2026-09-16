//! End-to-end coverage for the mission-reasoning compile/fallback/no-dispatch
//! contract that [`orchestrator_app::MissionService::admit_mission`] gates on.
//!
//! Compilation is the single hard precondition to admission: a malformed or
//! cyclic proposal fails to compile, so `admit_mission` returns an error before
//! it persists a reasoning row or reaches an executor. This target proves that
//! contract through the same `compile_proposal` seam `admit_mission` uses, plus
//! the deterministic offline keyword fallback and the app-level error surface.
//!
//! The reasoning *persistence* and *no-chain-of-thought* guarantees are proven
//! by the in-crate `mission_service` unit tests: the Rust runtime store is
//! foundation-only (production runtime-home enrollment is deliberately disabled,
//! and its boundary constructors are crate-internal), so a `RuntimeStore` cannot
//! be opened from an out-of-crate integration test. Those store-backed
//! assertions therefore run under `cargo test -p orchestrator-app --offline`
//! (the whole-crate gate), while this target covers the store-independent
//! contract.

use orchestrator_app::MissionServiceError;
use orchestrator_core::{
    AuthoredParseContext, MissionProposal, ProposalError, ProposedPhase, compile_proposal,
    keyword_fallback_proposal,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn proposed(name: &str, objective: &str, persona: &str, deps: &[&str]) -> ProposedPhase {
    ProposedPhase {
        name: name.to_owned(),
        objective: objective.to_owned(),
        persona: persona.to_owned(),
        skills: Vec::new(),
        depends_on: deps
            .iter()
            .map(|dependency| (*dependency).to_owned())
            .collect(),
    }
}

#[test]
fn typed_proposal_compiles_into_a_dependency_dag() -> TestResult {
    let proposal = MissionProposal {
        phases: vec![
            proposed("plan", "produce a build plan", "architect", &[]),
            proposed("build", "implement the plan", "engineer", &["plan"]),
            proposed(
                "review",
                "review the implementation",
                "reviewer",
                &["build"],
            ),
        ],
    };
    let plan = compile_proposal(&proposal, &AuthoredParseContext::default())?;
    assert_eq!(plan.phases.len(), 3);
    // Dependency names resolved to prior-phase ids, preserving order.
    assert_eq!(plan.phases[0].dependencies.len(), 0);
    assert_eq!(plan.phases[1].dependencies.len(), 1);
    assert_eq!(plan.phases[2].dependencies.len(), 1);
    assert_eq!(plan.phases[1].dependencies[0], plan.phases[0].id);
    assert_eq!(plan.phases[2].dependencies[0], plan.phases[1].id);
    Ok(())
}

#[test]
fn cyclic_proposal_does_not_compile_and_so_cannot_dispatch() {
    let cyclic = MissionProposal {
        phases: vec![
            proposed("a", "first", "p", &["b"]),
            proposed("b", "second", "p", &["a"]),
        ],
    };
    assert!(matches!(
        compile_proposal(&cyclic, &AuthoredParseContext::default()),
        Err(ProposalError::Cycle { .. })
    ));
}

#[test]
fn malformed_proposals_fail_closed_with_named_errors() {
    let context = AuthoredParseContext::default();

    assert!(
        matches!(
            compile_proposal(&MissionProposal::default(), &context),
            Err(ProposalError::Empty)
        ),
        "an empty proposal must not compile"
    );

    let missing = MissionProposal {
        phases: vec![proposed("only", "", "p", &[])],
    };
    assert!(
        matches!(
            compile_proposal(&missing, &context),
            Err(ProposalError::MissingField { .. })
        ),
        "a missing objective must not compile"
    );

    let duplicate = MissionProposal {
        phases: vec![
            proposed("dup", "x", "p", &[]),
            proposed("dup", "y", "p", &[]),
        ],
    };
    assert!(
        matches!(
            compile_proposal(&duplicate, &context),
            Err(ProposalError::DuplicateName { .. })
        ),
        "a duplicate phase name must not compile"
    );

    let unknown = MissionProposal {
        phases: vec![proposed("only", "x", "p", &["ghost"])],
    };
    assert!(
        matches!(
            compile_proposal(&unknown, &context),
            Err(ProposalError::UnknownDependency { .. })
        ),
        "an unknown dependency must not compile"
    );

    let self_dep = MissionProposal {
        phases: vec![proposed("solo", "x", "p", &["solo"])],
    };
    assert!(
        matches!(
            compile_proposal(&self_dep, &context),
            Err(ProposalError::SelfDependency { .. })
        ),
        "a self dependency must not compile"
    );
}

#[test]
fn oversize_json_capsule_fails_closed_before_parse() {
    let capsule = vec![b'{'; 128 * 1024];
    assert!(
        matches!(
            MissionProposal::from_json_bytes(&capsule),
            Err(ProposalError::Malformed)
        ),
        "an oversize capsule must fail closed"
    );
}

#[test]
fn keyword_fallback_is_deterministic_and_compiles_offline() -> TestResult {
    let task = "research the topic then test and verify the findings";
    let first = keyword_fallback_proposal(task);
    let second = keyword_fallback_proposal(task);
    assert_eq!(first, second, "the offline fallback must be deterministic");
    let plan = compile_proposal(&first, &AuthoredParseContext::default())?;
    assert!(!plan.phases.is_empty());
    Ok(())
}

#[test]
fn service_error_surfaces_the_proposal_error() {
    // The app-level error type transparently carries the compile failure that
    // aborts admission before any persistence or dispatch.
    let error: MissionServiceError = ProposalError::Empty.into();
    assert!(matches!(
        error,
        MissionServiceError::Proposal(ProposalError::Empty)
    ));
}
