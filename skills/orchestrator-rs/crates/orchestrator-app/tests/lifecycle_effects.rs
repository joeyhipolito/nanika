use orchestrator_app::{
    FixtureAdmissionPolicy, FixtureProjectionFault, FixtureWorkspaceSeed, FreshFixtureAuthority,
    IsolatedFixtureRoot, LifecycleCoordinator, LifecycleError, OverlayExit, OwnedChildState,
    RoleDenyPolicy, SettingsOverlay, WorkerRole, WorkspaceError,
};
use orchestrator_core::{
    CheckpointPhase, CheckpointPlan, CheckpointProjection, EventId, MissionId, MissionState,
    PhaseDefinition, PhaseId, ReducerInput, ReducerTransition, VerificationClass, VerificationMode,
    VerificationOutcome, WorkerId, decide_verification, encode_current_checkpoint, reduce,
};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
static CASE: AtomicU64 = AtomicU64::new(1);

struct FixtureCase {
    parent: PathBuf,
    root: PathBuf,
    authority: FreshFixtureAuthority,
}

impl Drop for FixtureCase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

#[derive(Default)]
struct ManualOwnedChildren(Cell<bool>);

impl ManualOwnedChildren {
    fn set(&self, unresolved: bool) {
        self.0.set(unresolved);
    }
}

impl OwnedChildState for ManualOwnedChildren {
    fn has_unresolved_children(&self) -> bool {
        self.0.get()
    }
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

fn private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

fn fixture_case(label: &str) -> TestResult<FixtureCase> {
    let number = CASE.fetch_add(1, Ordering::Relaxed);
    let temp = std::fs::canonicalize(std::env::temp_dir())?;
    let parent = temp.join(format!(
        "orchestrator-rs-lifecycle-{}-{number}-{label}",
        std::process::id()
    ));
    private_dir(&parent)?;
    let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
    let root = isolated.path().to_path_buf();
    let policy = FixtureAdmissionPolicy::new(
        parent.join("live-user"),
        std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?,
        &temp,
    );
    let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
    Ok(FixtureCase {
        parent,
        root,
        authority,
    })
}

fn input(
    id: &str,
    sequence: i64,
    phase: Option<&PhaseId>,
    transition: ReducerTransition,
) -> TestResult<ReducerInput> {
    let data = match &transition {
        ReducerTransition::PhaseFailed { error } => json!({"error": error}),
        ReducerTransition::PhaseSkipped { reason }
        | ReducerTransition::MissionCancelled { reason } => json!({"reason": reason}),
        _ => Value::Null,
    };
    Ok(ReducerInput {
        event_id: EventId::new(id)?,
        sequence,
        timestamp: format!("2026-07-13T00:00:{sequence:02}Z"),
        mission_id: MissionId::new("lifecycle-case")?,
        phase_id: phase.cloned(),
        worker_id: None,
        data,
        extra: BTreeMap::new(),
        transition,
    })
}

fn initial_state() -> TestResult<(MissionState, PhaseId)> {
    let mission = MissionId::new("lifecycle-case")?;
    let phase = PhaseId::new("verify")?;
    let state = MissionState::new(
        mission,
        vec![PhaseDefinition {
            id: phase.clone(),
            dependencies: Vec::new(),
        }],
    )?;
    Ok((state, phase))
}

fn checkpoint(state: &MissionState) -> CheckpointProjection {
    CheckpointProjection {
        workspace_id: "lifecycle-case".to_owned(),
        status: match state.status() {
            orchestrator_core::MissionStatus::NotStarted => "pending",
            orchestrator_core::MissionStatus::InProgress => "in_progress",
            orchestrator_core::MissionStatus::Completed => "completed",
            orchestrator_core::MissionStatus::Failed => "failed",
            orchestrator_core::MissionStatus::Cancelled => "cancelled",
        }
        .to_owned(),
        started_at: state.started_at().unwrap_or_default().to_owned(),
        plan: Some(CheckpointPlan {
            id: "plan".to_owned(),
            phases: state
                .phases()
                .map(|phase| CheckpointPhase {
                    id: phase.id.to_string(),
                    status: format!("{:?}", phase.status).to_ascii_lowercase(),
                    ..CheckpointPhase::default()
                })
                .collect(),
            ..CheckpointPlan::default()
        }),
        ..CheckpointProjection::default()
    }
}

fn coordinator(
    case: &FixtureCase,
    state: MissionState,
) -> TestResult<LifecycleCoordinator<ManualOwnedChildren>> {
    let checkpoint = checkpoint(&state);
    let workspace = case.authority.create_workspace(
        MissionId::new("lifecycle-case")?,
        FixtureWorkspaceSeed::new(b"fixture\n".to_vec(), &checkpoint, b"{}".to_vec())?,
    )?;
    Ok(LifecycleCoordinator::new(
        workspace,
        ManualOwnedChildren::default(),
        state,
    )?)
}

fn running_coordinator(
    case: &FixtureCase,
) -> TestResult<(LifecycleCoordinator<ManualOwnedChildren>, PhaseId)> {
    let (state, phase) = initial_state()?;
    let mut coordinator = coordinator(case, state)?;
    coordinator.transition(
        &input("seed-start", 1, None, ReducerTransition::MissionStarted)?,
        None,
    )?;
    coordinator.transition(
        &input(
            "seed-phase",
            2,
            Some(&phase),
            ReducerTransition::PhaseStarted,
        )?,
        None,
    )?;
    Ok((coordinator, phase))
}

fn pass() -> orchestrator_core::VerificationDecision {
    decide_verification(
        VerificationOutcome::Classified(VerificationClass::Pass),
        VerificationMode::Block,
    )
}

fn skip_warn() -> orchestrator_core::VerificationDecision {
    decide_verification(
        VerificationOutcome::Classified(VerificationClass::Skip),
        VerificationMode::Warn,
    )
}

fn workspace_root(case: &FixtureCase) -> PathBuf {
    case.root.join("workspaces/lifecycle-case")
}

#[test]
fn terminal_transition_table() -> TestResult {
    // LC-PHASE-SUCCESS / LC-EVENT-BEFORE-CHECKPOINT
    let case = fixture_case("phase-success")?;
    let (mut coordinator, phase) = running_coordinator(&case)?;
    let completion = input(
        "phase-complete",
        3,
        Some(&phase),
        ReducerTransition::PhaseCompleted,
    )?;
    let ack = coordinator.transition(&completion, Some(pass()))?;
    assert!(!ack.replayed, "LC-PHASE-SUCCESS");
    let root = workspace_root(&case);
    let event_bytes = std::fs::read(root.join("events.jsonl"))?;
    let checkpoint_bytes = std::fs::read(root.join("checkpoint.json"))?;
    assert!(event_bytes.ends_with(b"\n"));
    assert_eq!(
        orchestrator_core::decode_checkpoint(&checkpoint_bytes)?
            .projection
            .plan
            .ok_or("completed checkpoint is missing its plan")?
            .phases[0]
            .status,
        "completed",
        "completed checkpoint follows its event"
    );

    // LC-PHASE-CHILD-BLOCK and reset after closure.
    let case = fixture_case("phase-child")?;
    let (mut coordinator, phase) = running_coordinator(&case)?;
    coordinator.registry()?.set(true);
    let completion = input(
        "phase-complete",
        3,
        Some(&phase),
        ReducerTransition::PhaseCompleted,
    )?;
    let log_before = std::fs::read(workspace_root(&case).join("events.jsonl"))?;
    assert!(
        matches!(
            coordinator.transition(&completion, Some(pass())),
            Err(LifecycleError::UnresolvedChildren)
        ),
        "LC-PHASE-CHILD-BLOCK"
    );
    assert_eq!(
        std::fs::read(workspace_root(&case).join("events.jsonl"))?,
        log_before
    );
    coordinator.registry()?.set(false);
    coordinator.transition(&completion, Some(pass()))?;

    // LC-PHASE-NO-GATE / LC-PHASE-NONPASS-BLOCK (warn never promotes skip).
    for (label, decision) in [
        ("LC-PHASE-NO-GATE", None),
        ("LC-PHASE-NONPASS-BLOCK", Some(skip_warn())),
    ] {
        let case = fixture_case(label)?;
        let (mut coordinator, phase) = running_coordinator(&case)?;
        let completion = input(
            "phase-complete",
            3,
            Some(&phase),
            ReducerTransition::PhaseCompleted,
        )?;
        let log_before = std::fs::read(workspace_root(&case).join("events.jsonl"))?;
        assert!(
            matches!(
                coordinator.transition(&completion, decision),
                Err(LifecycleError::VerificationNotPassed)
            ),
            "{label}"
        );
        assert_eq!(
            std::fs::read(workspace_root(&case).join("events.jsonl"))?,
            log_before,
            "{label}"
        );
    }

    // LC-PHASE-FAILURE and LC-PHASE-SKIPPED are terminal but never success gates.
    for (label, transition) in [
        (
            "LC-PHASE-FAILURE",
            ReducerTransition::PhaseFailed {
                error: "failed".to_owned(),
            },
        ),
        (
            "LC-PHASE-SKIPPED",
            ReducerTransition::PhaseSkipped {
                reason: "required check skipped".to_owned(),
            },
        ),
    ] {
        let case = fixture_case(label)?;
        let (mut coordinator, phase) = running_coordinator(&case)?;
        let terminal = input("phase-terminal", 3, Some(&phase), transition)?;
        coordinator.transition(&terminal, None)?;
        assert_ne!(
            coordinator
                .state()
                .phase(&phase)
                .ok_or("terminal table phase is missing")?
                .status,
            orchestrator_core::PhaseStatus::Completed,
            "{label}"
        );
    }
    Ok(())
}

#[test]
fn mission_children_and_cancel_replay_table() -> TestResult {
    let case = fixture_case("mission-child")?;
    let (mut coordinator, phase) = running_coordinator(&case)?;
    coordinator.transition(
        &input(
            "seed-complete",
            3,
            Some(&phase),
            ReducerTransition::PhaseCompleted,
        )?,
        Some(pass()),
    )?;
    coordinator.registry()?.set(true);
    let log_before = std::fs::read(workspace_root(&case).join("events.jsonl"))?;
    let complete = input(
        "mission-complete",
        4,
        None,
        ReducerTransition::MissionCompleted,
    )?;
    assert!(
        matches!(
            coordinator.transition(&complete, None),
            Err(LifecycleError::UnresolvedChildren)
        ),
        "LC-MISSION-CHILD-BLOCK"
    );
    assert_eq!(
        std::fs::read(workspace_root(&case).join("events.jsonl"))?,
        log_before
    );

    let case = fixture_case("cancel-replay")?;
    let (mut coordinator, _) = running_coordinator(&case)?;
    let cancel = input(
        "mission-cancel",
        3,
        None,
        ReducerTransition::MissionCancelled {
            reason: "operator".to_owned(),
        },
    )?;
    let cancelled = coordinator.transition(&cancel, None)?;
    assert_eq!(
        cancelled.mission_status,
        orchestrator_core::MissionStatus::Cancelled,
        "LC-MISSION-CANCELLED"
    );
    let root = workspace_root(&case);
    let first_log = std::fs::read(root.join("events.jsonl"))?;
    private_file(
        &root.join(".events.jsonl.projection-tmp"),
        b"untouched replay sentinel",
    )?;
    let replay = coordinator.transition(&cancel, None)?;
    assert!(replay.replayed, "LC-CANCEL-REPLAY");
    assert_eq!(
        std::fs::read(root.join("events.jsonl"))?,
        first_log,
        "LC-CANCEL-REPLAY"
    );
    assert_eq!(
        std::fs::read(root.join(".events.jsonl.projection-tmp"))?,
        b"untouched replay sentinel",
        "LC-CANCEL-REPLAY"
    );
    assert!(
        matches!(coordinator.registry(), Err(LifecycleError::AdmissionClosed)),
        "LC-ADMISSION-CLOSED"
    );
    Ok(())
}

#[test]
fn overlay_reducer_error_and_exact_replay_are_non_mutating() -> TestResult {
    // LC-PHASE-OVERLAY-BLOCK
    let case = fixture_case("overlay")?;
    let (state, phase) = initial_state()?;
    let checkpoint = checkpoint(&state);
    let workspace = case.authority.create_workspace(
        MissionId::new("lifecycle-case")?,
        FixtureWorkspaceSeed::new(b"fixture\n".to_vec(), &checkpoint, b"{}".to_vec())?,
    )?;
    let target = case.authority.create_target("target")?;
    let overlay = SettingsOverlay::install(
        target,
        &workspace,
        &WorkerId::new("worker")?,
        RoleDenyPolicy::for_role(WorkerRole::Implementer),
    )?;
    let mut coordinator =
        LifecycleCoordinator::new(workspace, ManualOwnedChildren::default(), state)?;
    coordinator.attach_overlay(overlay)?;
    coordinator.transition(
        &input("seed-start", 1, None, ReducerTransition::MissionStarted)?,
        None,
    )?;
    coordinator.transition(
        &input(
            "seed-phase",
            2,
            Some(&phase),
            ReducerTransition::PhaseStarted,
        )?,
        None,
    )?;
    let completion = input(
        "phase-complete",
        3,
        Some(&phase),
        ReducerTransition::PhaseCompleted,
    )?;
    assert!(
        matches!(
            coordinator.transition(&completion, Some(pass())),
            Err(LifecycleError::OverlayUnresolved)
        ),
        "LC-PHASE-OVERLAY-BLOCK"
    );
    coordinator.restore_overlay(OverlayExit::Success)?;
    coordinator.transition(&completion, Some(pass()))?;

    // LC-REDUCER-ERROR: mission completion while work is still running cannot mutate state/files.
    let case = fixture_case("reducer-error")?;
    let (mut coordinator, _) = running_coordinator(&case)?;
    let original_state = coordinator.state().clone();
    let root = workspace_root(&case);
    let checkpoint_before = std::fs::read(root.join("checkpoint.json"))?;
    let events_before = std::fs::read(root.join("events.jsonl"))?;
    let illegal = input(
        "mission-complete",
        3,
        None,
        ReducerTransition::MissionCompleted,
    )?;
    assert!(
        matches!(
            coordinator.transition(&illegal, None),
            Err(LifecycleError::Reducer(_))
        ),
        "LC-REDUCER-ERROR"
    );
    assert_eq!(coordinator.state(), &original_state, "LC-REDUCER-ERROR");
    assert_eq!(
        std::fs::read(root.join("checkpoint.json"))?,
        checkpoint_before,
        "LC-REDUCER-ERROR"
    );
    assert_eq!(
        std::fs::read(root.join("events.jsonl"))?,
        events_before,
        "LC-REDUCER-ERROR"
    );

    // LC-EXACT-REPLAY: exact success replay bypasses projection entirely.
    let case = fixture_case("exact-replay")?;
    let (mut coordinator, phase) = running_coordinator(&case)?;
    let completion = input(
        "phase-complete",
        3,
        Some(&phase),
        ReducerTransition::PhaseCompleted,
    )?;
    coordinator.transition(&completion, Some(pass()))?;
    let root = workspace_root(&case);
    let first_log = std::fs::read(root.join("events.jsonl"))?;
    private_file(
        &root.join(".checkpoint.json.projection-tmp"),
        b"replay sentinel",
    )?;
    let replay = coordinator.transition(&completion, Some(pass()))?;
    assert!(replay.replayed, "LC-EXACT-REPLAY");
    assert_eq!(
        std::fs::read(root.join("events.jsonl"))?,
        first_log,
        "LC-EXACT-REPLAY"
    );
    assert_eq!(
        std::fs::read(root.join(".checkpoint.json.projection-tmp"))?,
        b"replay sentinel",
        "LC-EXACT-REPLAY"
    );
    Ok(())
}

#[test]
fn partial_event_projection_repairs_checkpoint_without_duplicate() -> TestResult {
    let case = fixture_case("partial")?;
    let (coordinator, phase) = running_coordinator(&case)?;
    let state = coordinator.state().clone();
    let completion = input(
        "phase-complete",
        3,
        Some(&phase),
        ReducerTransition::PhaseCompleted,
    )?;
    let next_state = reduce(&state, &completion)?.state;
    let next_checkpoint = encode_current_checkpoint(&checkpoint(&next_state))?;
    let event = br#"{"id":"phase-complete","type":"phase.completed","timestamp":"2026-07-13T00:00:03Z","sequence":3,"mission_id":"lifecycle-case","phase_id":"verify"}"#;
    let root = workspace_root(&case);
    let mut event_log = std::fs::read(root.join("events.jsonl"))?;
    event_log.extend_from_slice(event);
    event_log.push(b'\n');
    private_file(&root.join("events.jsonl"), &event_log)?;
    private_file(
        &root.join(".checkpoint.json.projection-tmp"),
        &next_checkpoint,
    )?;

    drop(coordinator);
    let workspace = case
        .authority
        .open_workspace(MissionId::new("lifecycle-case")?)?;
    let (initial, _) = initial_state()?;
    let mut coordinator =
        LifecycleCoordinator::new(workspace, ManualOwnedChildren::default(), initial)?;
    assert_eq!(
        coordinator
            .state()
            .phase(&phase)
            .ok_or("recovery phase is missing")?
            .status,
        orchestrator_core::PhaseStatus::Running,
        "LC-RECOVERY-STATE"
    );
    assert!(
        matches!(coordinator.registry(), Err(LifecycleError::RecoveryPending)),
        "LC-RECOVERY-ADMISSION"
    );
    let checkpoint_before = std::fs::read(root.join("checkpoint.json"))?;
    assert!(
        matches!(
            coordinator.transition(&completion, None),
            Err(LifecycleError::VerificationNotPassed)
        ),
        "LC-RECOVERY-GATE"
    );
    assert_eq!(
        std::fs::read(root.join("checkpoint.json"))?,
        checkpoint_before,
        "failed recovery gate must not mutate the checkpoint"
    );
    coordinator.transition(&completion, Some(pass()))?;
    assert_eq!(
        std::fs::read(root.join("events.jsonl"))?,
        event_log,
        "LC-PARTIAL-RECOVERY"
    );
    assert_eq!(
        std::fs::read(root.join("checkpoint.json"))?,
        next_checkpoint,
        "LC-PARTIAL-RECOVERY"
    );
    assert!(
        !root.join(".checkpoint.json.projection-tmp").exists(),
        "LC-PARTIAL-RECOVERY"
    );
    Ok(())
}

#[test]
fn injected_projection_boundaries_recover_before_acknowledgement() -> TestResult {
    let case = fixture_case("same-instance-recovery")?;
    let (mut coordinator, phase) = running_coordinator(&case)?;
    let completion = input(
        "phase-complete",
        3,
        Some(&phase),
        ReducerTransition::PhaseCompleted,
    )?;
    coordinator.inject_fixture_projection_fault_once(FixtureProjectionFault::AfterEventSync);
    assert!(matches!(
        coordinator.transition(&completion, Some(pass())),
        Err(LifecycleError::Workspace(
            WorkspaceError::InjectedProjectionFault("after-event-sync")
        ))
    ));
    let root = workspace_root(&case);
    let event_only_log = std::fs::read(root.join("events.jsonl"))?;
    assert_eq!(
        event_only_log
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .count(),
        3,
        "LC-EVENT-BEFORE-CHECKPOINT"
    );
    assert_eq!(
        orchestrator_core::decode_checkpoint(&std::fs::read(root.join("checkpoint.json"))?)?
            .projection
            .plan
            .ok_or("event-only checkpoint is missing its plan")?
            .phases[0]
            .status,
        "running",
        "LC-EVENT-BEFORE-CHECKPOINT"
    );
    assert_eq!(
        coordinator
            .state()
            .phase(&phase)
            .ok_or("event-only phase is missing")?
            .status,
        orchestrator_core::PhaseStatus::Running
    );
    assert!(matches!(
        coordinator.registry(),
        Err(LifecycleError::RecoveryPending)
    ));
    let unrelated = input(
        "unrelated-while-pending",
        4,
        None,
        ReducerTransition::MissionCancelled {
            reason: "unrelated".to_owned(),
        },
    )?;
    assert!(matches!(
        coordinator.transition(&unrelated, None),
        Err(LifecycleError::RecoveryPending)
    ));
    coordinator.transition(&completion, Some(pass()))?;
    assert_eq!(
        std::fs::read(root.join("events.jsonl"))?,
        event_only_log,
        "LC-SAME-INSTANCE-RECOVERY"
    );
    assert_eq!(
        coordinator
            .state()
            .phase(&phase)
            .ok_or("same-instance recovery phase is missing")?
            .status,
        orchestrator_core::PhaseStatus::Completed,
        "LC-SAME-INSTANCE-RECOVERY"
    );

    let case = fixture_case("restart-resync")?;
    let (mut coordinator, phase) = running_coordinator(&case)?;
    let completion = input(
        "phase-complete",
        3,
        Some(&phase),
        ReducerTransition::PhaseCompleted,
    )?;
    coordinator.inject_fixture_projection_fault_once(FixtureProjectionFault::AfterCheckpointRename);
    assert!(matches!(
        coordinator.transition(&completion, Some(pass())),
        Err(LifecycleError::Workspace(
            WorkspaceError::InjectedProjectionFault("after-checkpoint-rename")
        ))
    ));
    assert_eq!(
        coordinator
            .state()
            .phase(&phase)
            .ok_or("checkpoint-renamed phase is missing")?
            .status,
        orchestrator_core::PhaseStatus::Running
    );
    drop(coordinator);
    let workspace = case
        .authority
        .open_workspace(MissionId::new("lifecycle-case")?)?;
    let (initial, _) = initial_state()?;
    let recovered = LifecycleCoordinator::new(workspace, ManualOwnedChildren::default(), initial)?;
    assert_eq!(
        recovered
            .state()
            .phase(&phase)
            .ok_or("restart recovery phase is missing")?
            .status,
        orchestrator_core::PhaseStatus::Completed,
        "LC-RESTART-RESYNC"
    );
    Ok(())
}

#[test]
fn post_publication_conflict_and_identity_failures_close_admission() -> TestResult {
    for (label, fault) in [
        (
            "post-publish-conflict",
            FixtureProjectionFault::AfterCheckpointRenameConflict,
        ),
        (
            "post-publish-identity",
            FixtureProjectionFault::AfterCheckpointRenameIdentityChange,
        ),
    ] {
        let case = fixture_case(label)?;
        let (mut coordinator, phase) = running_coordinator(&case)?;
        let completion = input(
            "phase-complete",
            3,
            Some(&phase),
            ReducerTransition::PhaseCompleted,
        )?;
        coordinator.inject_fixture_projection_fault_once(fault);

        let result = coordinator.transition(&completion, Some(pass()));
        assert!(
            matches!(
                result,
                Err(LifecycleError::Workspace(
                    WorkspaceError::ProjectionIndeterminate { .. }
                ))
            ),
            "{label}: {result:?}"
        );
        assert!(matches!(
            coordinator.registry(),
            Err(LifecycleError::RecoveryPending)
        ));
        let mut closure_ran = false;
        let worker = coordinator.with_attempt(&phase, "blocked-worker", 1, |_, _| {
            closure_ran = true;
        });
        assert!(matches!(worker, Err(LifecycleError::RecoveryPending)));
        assert!(!closure_ran);

        if fault == FixtureProjectionFault::AfterCheckpointRenameIdentityChange {
            std::fs::remove_dir(workspace_root(&case))?;
            std::fs::rename(
                case.root
                    .join("workspaces/.lifecycle-case.projection-identity-fault"),
                workspace_root(&case),
            )?;
            coordinator.transition(&completion, Some(pass()))?;
            let mission = input(
                "mission-complete",
                4,
                None,
                ReducerTransition::MissionCompleted,
            )?;
            coordinator.transition(&mission, None)?;
        }
    }
    Ok(())
}

#[test]
fn constructor_rejects_pre_reduced_state_and_conflicting_recovery() -> TestResult {
    let case = fixture_case("pre-reduced")?;
    let (initial, _) = initial_state()?;
    let running = reduce(
        &initial,
        &input("seed-start", 1, None, ReducerTransition::MissionStarted)?,
    )?
    .state;
    let workspace = case.authority.create_workspace(
        MissionId::new("lifecycle-case")?,
        FixtureWorkspaceSeed::new(b"fixture\n".to_vec(), &checkpoint(&running), b"{}".to_vec())?,
    )?;
    assert!(
        matches!(
            LifecycleCoordinator::new(workspace, ManualOwnedChildren::default(), running),
            Err(LifecycleError::InitialStateNotPristine)
        ),
        "LC-PRISTINE-ONLY"
    );

    let case = fixture_case("recovery-conflict")?;
    let (coordinator, _) = running_coordinator(&case)?;
    let mut conflict = checkpoint(coordinator.state());
    conflict.status = "failed".to_owned();
    let root = workspace_root(&case);
    private_file(
        &root.join("checkpoint.json"),
        &encode_current_checkpoint(&conflict)?,
    )?;
    drop(coordinator);
    let workspace = case
        .authority
        .open_workspace(MissionId::new("lifecycle-case")?)?;
    let (initial, _) = initial_state()?;
    assert!(
        matches!(
            LifecycleCoordinator::new(workspace, ManualOwnedChildren::default(), initial),
            Err(LifecycleError::RecoveryConflict)
        ),
        "LC-RECOVERY-CONFLICT"
    );

    let case = fixture_case("projection-conflict")?;
    let (mut coordinator, phase) = running_coordinator(&case)?;
    let state_before = coordinator.state().clone();
    let root = workspace_root(&case);
    let checkpoint_before = std::fs::read(root.join("checkpoint.json"))?;
    let mut changed_log = std::fs::read(root.join("events.jsonl"))?;
    changed_log.extend_from_slice(
        br#"{"id":"external","type":"phase.failed","timestamp":"2026-07-13T00:00:09Z","sequence":9,"mission_id":"lifecycle-case","phase_id":"verify","data":{"error":"external"}}"#,
    );
    changed_log.push(b'\n');
    private_file(&root.join("events.jsonl"), &changed_log)?;
    let completion = input(
        "phase-complete",
        3,
        Some(&phase),
        ReducerTransition::PhaseCompleted,
    )?;
    assert!(
        matches!(
            coordinator.transition(&completion, Some(pass())),
            Err(LifecycleError::Workspace(
                WorkspaceError::ProjectionConflict
            ))
        ),
        "LC-PROJECTION-CONFLICT"
    );
    assert_eq!(coordinator.state(), &state_before);
    assert_eq!(
        std::fs::read(root.join("checkpoint.json"))?,
        checkpoint_before
    );
    assert_eq!(std::fs::read(root.join("events.jsonl"))?, changed_log);

    let case = fixture_case("pending-duplicate")?;
    let (coordinator, phase) = running_coordinator(&case)?;
    let completion = input(
        "phase-complete",
        3,
        Some(&phase),
        ReducerTransition::PhaseCompleted,
    )?;
    let event = br#"{"id":"phase-complete","type":"phase.completed","timestamp":"2026-07-13T00:00:03Z","sequence":3,"mission_id":"lifecycle-case","phase_id":"verify"}"#;
    let root = workspace_root(&case);
    let mut pending_log = std::fs::read(root.join("events.jsonl"))?;
    pending_log.extend_from_slice(event);
    pending_log.push(b'\n');
    private_file(&root.join("events.jsonl"), &pending_log)?;
    drop(coordinator);
    let workspace = case
        .authority
        .open_workspace(MissionId::new("lifecycle-case")?)?;
    let (initial, _) = initial_state()?;
    let mut coordinator =
        LifecycleCoordinator::new(workspace, ManualOwnedChildren::default(), initial)?;
    let checkpoint_before = std::fs::read(root.join("checkpoint.json"))?;
    let mut duplicated_log = pending_log;
    duplicated_log.extend_from_slice(event);
    duplicated_log.push(b'\n');
    private_file(&root.join("events.jsonl"), &duplicated_log)?;
    assert!(
        matches!(
            coordinator.transition(&completion, Some(pass())),
            Err(LifecycleError::Workspace(
                WorkspaceError::ProjectionConflict
            ))
        ),
        "LC-PENDING-DUPLICATE-CONFLICT"
    );
    assert_eq!(
        coordinator
            .state()
            .phase(&phase)
            .ok_or("conflicting recovery phase is missing")?
            .status,
        orchestrator_core::PhaseStatus::Running
    );
    assert_eq!(
        std::fs::read(root.join("checkpoint.json"))?,
        checkpoint_before
    );
    assert_eq!(std::fs::read(root.join("events.jsonl"))?, duplicated_log);
    Ok(())
}
