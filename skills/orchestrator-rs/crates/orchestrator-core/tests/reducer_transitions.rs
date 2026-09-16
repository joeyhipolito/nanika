use orchestrator_core::{
    EventId, MissionId, MissionState, MissionStateBuildError, MissionStatus, PhaseDefinition,
    PhaseId, PhaseStatus, ReducerInput, ReducerTransition, Reduction, TransitionError, reduce,
};
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn phase(value: &str) -> Result<PhaseId, Box<dyn std::error::Error>> {
    Ok(PhaseId::new(value)?)
}

fn definition(
    id: &str,
    dependencies: &[&str],
) -> Result<PhaseDefinition, Box<dyn std::error::Error>> {
    Ok(PhaseDefinition {
        id: phase(id)?,
        dependencies: dependencies
            .iter()
            .map(|dependency| phase(dependency))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn plan() -> Result<MissionState, Box<dyn std::error::Error>> {
    Ok(MissionState::new(
        MissionId::new("mission-1")?,
        vec![
            definition("root", &[])?,
            definition("left", &["root"])?,
            definition("right", &["root"])?,
            definition("leaf", &["left", "right"])?,
        ],
    )?)
}

fn input(
    sequence: i64,
    phase_id: Option<&str>,
    transition: ReducerTransition,
) -> Result<ReducerInput, Box<dyn std::error::Error>> {
    Ok(ReducerInput {
        event_id: EventId::new(format!("event-{sequence}"))?,
        sequence,
        timestamp: format!("2026-07-13T00:00:{sequence:02}Z"),
        mission_id: MissionId::new("mission-1")?,
        phase_id: phase_id.map(phase).transpose()?,
        worker_id: None,
        data: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
        transition,
    })
}

fn apply(
    state: MissionState,
    sequence: i64,
    phase_id: Option<&str>,
    transition: ReducerTransition,
) -> Result<Reduction, Box<dyn std::error::Error>> {
    Ok(reduce(&state, &input(sequence, phase_id, transition)?)?)
}

fn phase_status(state: &MissionState, phase_id: &str) -> Result<PhaseStatus, String> {
    let id = PhaseId::new(phase_id).map_err(|error| error.to_string())?;
    state
        .phase(&id)
        .map(|phase| phase.status)
        .ok_or_else(|| format!("missing test phase {phase_id}"))
}

#[test]
fn legal_transition_table_covers_success_retry_release_failure_skip_and_cancel() -> TestResult {
    let mut success = apply(plan()?, 1, None, ReducerTransition::MissionStarted)?.state;
    assert_eq!(success.status(), MissionStatus::InProgress);
    assert_eq!(success.started_at(), Some("2026-07-13T00:00:01Z"));

    success = apply(success, 2, Some("root"), ReducerTransition::PhaseStarted)?.state;
    success = apply(success, 3, Some("root"), ReducerTransition::PhaseRetrying)?.state;
    let root = success
        .phase(&phase("root")?)
        .ok_or("root phase disappeared")?;
    assert_eq!(root.status, PhaseStatus::Running);
    assert_eq!(root.retry_observations, 1);
    assert_eq!(root.started_at.as_deref(), Some("2026-07-13T00:00:02Z"));
    assert_eq!(root.finished_at, None);
    assert_eq!(root.error, None);
    assert_eq!(root.skip_reason, None);

    let completed_root = apply(success, 4, Some("root"), ReducerTransition::PhaseCompleted)?;
    assert_eq!(
        completed_root.released_phases,
        [phase("left")?, phase("right")?],
        "release order follows the authored plan"
    );
    success = completed_root.state;
    for (sequence, phase_id, transition) in [
        (5, "left", ReducerTransition::PhaseStarted),
        (6, "left", ReducerTransition::PhaseCompleted),
        (7, "right", ReducerTransition::PhaseStarted),
    ] {
        success = apply(success, sequence, Some(phase_id), transition)?.state;
    }
    let completed_right = apply(success, 8, Some("right"), ReducerTransition::PhaseCompleted)?;
    assert_eq!(completed_right.released_phases, [phase("leaf")?]);
    success = apply(
        completed_right.state,
        9,
        Some("leaf"),
        ReducerTransition::PhaseStarted,
    )?
    .state;
    success = apply(success, 10, Some("leaf"), ReducerTransition::PhaseCompleted)?.state;
    success = apply(success, 11, None, ReducerTransition::MissionCompleted)?.state;
    assert_eq!(success.status(), MissionStatus::Completed);
    assert_eq!(success.finished_at(), Some("2026-07-13T00:00:11Z"));

    let mut failed = apply(plan()?, 1, None, ReducerTransition::MissionStarted)?.state;
    failed = apply(failed, 2, Some("root"), ReducerTransition::PhaseStarted)?.state;
    failed = apply(failed, 3, Some("root"), ReducerTransition::PhaseCompleted)?.state;
    failed = apply(failed, 4, Some("left"), ReducerTransition::PhaseStarted)?.state;
    failed = apply(
        failed,
        5,
        Some("left"),
        ReducerTransition::PhaseFailed {
            error: "compiler error".to_owned(),
        },
    )?
    .state;
    assert_eq!(phase_status(&failed, "left")?, PhaseStatus::Failed);
    assert_eq!(phase_status(&failed, "leaf")?, PhaseStatus::Skipped);
    assert_eq!(phase_status(&failed, "right")?, PhaseStatus::Pending);
    failed = apply(failed, 6, None, ReducerTransition::MissionFailed)?.state;
    assert_eq!(failed.status(), MissionStatus::Failed);
    assert_eq!(phase_status(&failed, "right")?, PhaseStatus::Skipped);

    let skipped = apply(
        plan()?,
        1,
        Some("root"),
        ReducerTransition::PhaseSkipped {
            reason: "not required".to_owned(),
        },
    )?
    .state;
    assert!(
        skipped
            .phases()
            .all(|phase| phase.status == PhaseStatus::Skipped),
        "dependency skipping propagates transitively before mission start"
    );
    let skipped = apply(skipped, 2, None, ReducerTransition::MissionStarted)?.state;
    let skipped = apply(skipped, 3, None, ReducerTransition::MissionCompleted)?.state;
    assert_eq!(skipped.status(), MissionStatus::Completed);

    let cancelled = apply(
        plan()?,
        1,
        None,
        ReducerTransition::MissionCancelled {
            reason: "operator request".to_owned(),
        },
    )?
    .state;
    assert_eq!(cancelled.status(), MissionStatus::Cancelled);
    assert!(
        cancelled
            .phases()
            .all(|phase| phase.status == PhaseStatus::Skipped)
    );

    let running_cancel = apply(plan()?, 1, None, ReducerTransition::MissionStarted)?.state;
    let running_cancel = apply(
        running_cancel,
        2,
        Some("root"),
        ReducerTransition::PhaseStarted,
    )?
    .state;
    let running_cancel = apply(
        running_cancel,
        3,
        None,
        ReducerTransition::MissionCancelled {
            reason: "operator request".to_owned(),
        },
    )?
    .state;
    assert_eq!(running_cancel.status(), MissionStatus::Cancelled);
    assert_eq!(phase_status(&running_cancel, "root")?, PhaseStatus::Skipped);

    let running_skip = apply(plan()?, 1, None, ReducerTransition::MissionStarted)?.state;
    let running_skip = apply(
        running_skip,
        2,
        Some("root"),
        ReducerTransition::PhaseStarted,
    )?
    .state;
    let running_skip = apply(
        running_skip,
        3,
        Some("root"),
        ReducerTransition::PhaseSkipped {
            reason: "superseded".to_owned(),
        },
    )?
    .state;
    assert!(
        running_skip
            .phases()
            .all(|phase| phase.status == PhaseStatus::Skipped)
    );
    Ok(())
}

#[test]
fn exact_replays_unknown_events_and_sequence_rules_are_deterministic() -> TestResult {
    let initial = plan()?;
    let started = input(1, None, ReducerTransition::MissionStarted)?;
    let first = reduce(&initial, &started)?;
    assert_eq!(reduce(&initial, &started)?, first);
    let replay = reduce(&first.state, &started)?;
    assert_eq!(replay.state, first.state);
    assert!(replay.released_phases.is_empty());

    let unknown = input(
        5,
        None,
        ReducerTransition::Unknown {
            event_type: "future.observation".to_owned(),
            data: json!({"retained": true}),
        },
    )?;
    let observed = reduce(&replay.state, &unknown)?;
    assert_eq!(observed.state.status(), MissionStatus::InProgress);
    assert_eq!(observed.state.greatest_applied_sequence(), Some(5));
    assert!(observed.state.applied_event(&unknown.event_id).is_some());

    let mut conflicting_id = unknown.clone();
    conflicting_id.timestamp = "2026-07-13T01:00:00Z".to_owned();
    let before_identity_conflict = observed.state.clone();
    assert_eq!(
        reduce(&observed.state, &conflicting_id),
        Err(TransitionError::EventIdentityConflict {
            event_id: unknown.event_id.clone()
        })
    );
    assert_eq!(observed.state, before_identity_conflict);

    let mut conflicting_sequence = input(5, None, ReducerTransition::MissionCompleted)?;
    conflicting_sequence.event_id = EventId::new("another-event")?;
    let before_sequence_conflict = observed.state.clone();
    assert_eq!(
        reduce(&observed.state, &conflicting_sequence),
        Err(TransitionError::SequenceConflict {
            sequence: 5,
            existing_event_id: unknown.event_id.clone()
        })
    );
    assert_eq!(observed.state, before_sequence_conflict);
    let before_out_of_order = observed.state.clone();
    assert_eq!(
        reduce(
            &observed.state,
            &input(4, Some("root"), ReducerTransition::PhaseStarted)?
        ),
        Err(TransitionError::OutOfOrder {
            sequence: 4,
            greatest: 5
        })
    );
    assert_eq!(observed.state, before_out_of_order);
    Ok(())
}

#[test]
fn invalid_transition_table_never_mutates_the_caller_state() -> TestResult {
    struct Case {
        state: MissionState,
        input: ReducerInput,
        expected: TransitionError,
    }

    let initial = plan()?;
    let started = apply(initial.clone(), 1, None, ReducerTransition::MissionStarted)?.state;
    let root_running = apply(
        started.clone(),
        2,
        Some("root"),
        ReducerTransition::PhaseStarted,
    )?
    .state;
    let root_done = apply(
        root_running.clone(),
        3,
        Some("root"),
        ReducerTransition::PhaseCompleted,
    )?
    .state;
    let failed_phase = apply(plan()?, 1, None, ReducerTransition::MissionStarted)?.state;
    let failed_phase = apply(
        failed_phase,
        2,
        Some("root"),
        ReducerTransition::PhaseStarted,
    )?
    .state;
    let skipped_dependency = apply(
        plan()?,
        1,
        Some("root"),
        ReducerTransition::PhaseSkipped {
            reason: "blocked".to_owned(),
        },
    )?
    .state;
    let skipped_dependency = apply(
        skipped_dependency,
        2,
        None,
        ReducerTransition::MissionStarted,
    )?
    .state;
    let failed_phase = apply(
        failed_phase,
        3,
        Some("root"),
        ReducerTransition::PhaseFailed {
            error: "failed".to_owned(),
        },
    )?
    .state;
    let terminal = apply(
        initial.clone(),
        1,
        None,
        ReducerTransition::MissionCancelled {
            reason: "stop".to_owned(),
        },
    )?
    .state;

    let mut wrong_mission = input(1, None, ReducerTransition::MissionStarted)?;
    wrong_mission.mission_id = MissionId::new("mission-2")?;
    let cases = vec![
        Case {
            state: initial.clone(),
            input: wrong_mission,
            expected: TransitionError::MissionMismatch {
                expected: MissionId::new("mission-1")?,
                found: MissionId::new("mission-2")?,
            },
        },
        Case {
            state: initial.clone(),
            input: input(1, Some("root"), ReducerTransition::PhaseStarted)?,
            expected: TransitionError::MissionNotInProgress {
                status: MissionStatus::NotStarted,
            },
        },
        Case {
            state: initial.clone(),
            input: input(1, None, ReducerTransition::MissionCompleted)?,
            expected: TransitionError::MissionNotInProgress {
                status: MissionStatus::NotStarted,
            },
        },
        Case {
            state: initial.clone(),
            input: input(1, None, ReducerTransition::MissionFailed)?,
            expected: TransitionError::MissionNotInProgress {
                status: MissionStatus::NotStarted,
            },
        },
        Case {
            state: started.clone(),
            input: input(2, None, ReducerTransition::MissionStarted)?,
            expected: TransitionError::AlreadyStarted,
        },
        Case {
            state: started.clone(),
            input: input(2, None, ReducerTransition::MissionCompleted)?,
            expected: TransitionError::MissionPhasesIncomplete,
        },
        Case {
            state: failed_phase.clone(),
            input: input(4, None, ReducerTransition::MissionCompleted)?,
            expected: TransitionError::MissionHasFailedPhase,
        },
        Case {
            state: started.clone(),
            input: input(2, Some("left"), ReducerTransition::PhaseStarted)?,
            expected: TransitionError::DependencyNotReady {
                phase_id: phase("left")?,
                dependency: phase("root")?,
            },
        },
        Case {
            state: root_running.clone(),
            input: input(3, Some("left"), ReducerTransition::PhaseStarted)?,
            expected: TransitionError::DependencyNotReady {
                phase_id: phase("left")?,
                dependency: phase("root")?,
            },
        },
        Case {
            state: failed_phase.clone(),
            input: input(4, Some("left"), ReducerTransition::PhaseStarted)?,
            expected: TransitionError::PhaseTerminal {
                phase_id: phase("left")?,
                status: PhaseStatus::Skipped,
            },
        },
        Case {
            state: skipped_dependency,
            input: input(3, Some("left"), ReducerTransition::PhaseStarted)?,
            expected: TransitionError::PhaseTerminal {
                phase_id: phase("left")?,
                status: PhaseStatus::Skipped,
            },
        },
        Case {
            state: started.clone(),
            input: input(2, Some("root"), ReducerTransition::PhaseCompleted)?,
            expected: TransitionError::PhaseNotRunning {
                phase_id: phase("root")?,
                status: PhaseStatus::Pending,
            },
        },
        Case {
            state: started.clone(),
            input: input(
                2,
                Some("root"),
                ReducerTransition::PhaseFailed {
                    error: "early".to_owned(),
                },
            )?,
            expected: TransitionError::PhaseNotRunning {
                phase_id: phase("root")?,
                status: PhaseStatus::Pending,
            },
        },
        Case {
            state: started.clone(),
            input: input(2, Some("root"), ReducerTransition::PhaseRetrying)?,
            expected: TransitionError::PhaseNotRunning {
                phase_id: phase("root")?,
                status: PhaseStatus::Pending,
            },
        },
        Case {
            state: root_running.clone(),
            input: input(3, Some("root"), ReducerTransition::PhaseStarted)?,
            expected: TransitionError::PhaseAlreadyRunning {
                phase_id: phase("root")?,
            },
        },
        Case {
            state: root_done,
            input: input(4, Some("root"), ReducerTransition::PhaseStarted)?,
            expected: TransitionError::PhaseTerminal {
                phase_id: phase("root")?,
                status: PhaseStatus::Completed,
            },
        },
        Case {
            state: started.clone(),
            input: input(2, None, ReducerTransition::PhaseStarted)?,
            expected: TransitionError::MissingPhaseId,
        },
        Case {
            state: started,
            input: input(2, Some("absent"), ReducerTransition::PhaseStarted)?,
            expected: TransitionError::UnknownPhase {
                phase_id: phase("absent")?,
            },
        },
        Case {
            state: terminal,
            input: input(2, None, ReducerTransition::MissionStarted)?,
            expected: TransitionError::MissionTerminal {
                status: MissionStatus::Cancelled,
            },
        },
    ];

    for case in cases {
        let before = case.state.clone();
        assert_eq!(reduce(&case.state, &case.input), Err(case.expected));
        assert_eq!(case.state, before, "an error mutated caller-owned state");
    }
    Ok(())
}

#[test]
fn every_new_phase_lifecycle_event_rejects_each_terminal_phase_status() -> TestResult {
    let started = apply(plan()?, 1, None, ReducerTransition::MissionStarted)?.state;
    let running = apply(
        started.clone(),
        2,
        Some("root"),
        ReducerTransition::PhaseStarted,
    )?
    .state;
    let completed = apply(
        running.clone(),
        3,
        Some("root"),
        ReducerTransition::PhaseCompleted,
    )?
    .state;
    let failed = apply(
        running,
        3,
        Some("root"),
        ReducerTransition::PhaseFailed {
            error: "failed".to_owned(),
        },
    )?
    .state;
    let skipped = apply(
        started,
        2,
        Some("root"),
        ReducerTransition::PhaseSkipped {
            reason: "skipped".to_owned(),
        },
    )?
    .state;

    for (state, sequence, status) in [
        (completed, 4, PhaseStatus::Completed),
        (failed, 4, PhaseStatus::Failed),
        (skipped, 3, PhaseStatus::Skipped),
    ] {
        for transition in [
            ReducerTransition::PhaseStarted,
            ReducerTransition::PhaseCompleted,
            ReducerTransition::PhaseFailed {
                error: "again".to_owned(),
            },
            ReducerTransition::PhaseSkipped {
                reason: "again".to_owned(),
            },
            ReducerTransition::PhaseRetrying,
        ] {
            let before = state.clone();
            assert_eq!(
                reduce(&state, &input(sequence, Some("root"), transition)?),
                Err(TransitionError::PhaseTerminal {
                    phase_id: phase("root")?,
                    status,
                })
            );
            assert_eq!(state, before);
        }
    }
    Ok(())
}

#[test]
fn terminal_state_is_monotonic_but_unknown_observations_and_exact_replays_survive() -> TestResult {
    for terminal_transition in [
        ReducerTransition::MissionFailed,
        ReducerTransition::MissionCancelled {
            reason: "cancel".to_owned(),
        },
    ] {
        let started = apply(plan()?, 1, None, ReducerTransition::MissionStarted)?.state;
        let terminal_input = input(2, None, terminal_transition)?;
        let terminal = reduce(&started, &terminal_input)?.state;
        let snapshot = terminal.clone();
        for transition in [
            ReducerTransition::MissionStarted,
            ReducerTransition::MissionCompleted,
            ReducerTransition::MissionFailed,
            ReducerTransition::MissionCancelled {
                reason: "again".to_owned(),
            },
            ReducerTransition::PhaseStarted,
            ReducerTransition::PhaseCompleted,
            ReducerTransition::PhaseFailed {
                error: "late".to_owned(),
            },
            ReducerTransition::PhaseSkipped {
                reason: "late".to_owned(),
            },
            ReducerTransition::PhaseRetrying,
        ] {
            let phase_id = matches!(
                transition,
                ReducerTransition::PhaseStarted
                    | ReducerTransition::PhaseCompleted
                    | ReducerTransition::PhaseFailed { .. }
                    | ReducerTransition::PhaseSkipped { .. }
                    | ReducerTransition::PhaseRetrying
            )
            .then_some("root");
            assert!(matches!(
                reduce(&terminal, &input(3, phase_id, transition)?),
                Err(TransitionError::MissionTerminal { .. })
            ));
            assert_eq!(terminal, snapshot);
        }
        assert_eq!(reduce(&terminal, &terminal_input)?.state, terminal);
        let unknown = reduce(
            &terminal,
            &input(
                3,
                None,
                ReducerTransition::Unknown {
                    event_type: "future.event".to_owned(),
                    data: json!({}),
                },
            )?,
        )?;
        assert_eq!(unknown.state.status(), terminal.status());
        assert_eq!(unknown.state.greatest_applied_sequence(), Some(3));
    }

    let completed = apply(
        plan()?,
        1,
        Some("root"),
        ReducerTransition::PhaseSkipped {
            reason: "not required".to_owned(),
        },
    )?
    .state;
    let completed = apply(completed, 2, None, ReducerTransition::MissionStarted)?.state;
    let completion = input(3, None, ReducerTransition::MissionCompleted)?;
    let completed = reduce(&completed, &completion)?.state;
    let snapshot = completed.clone();
    assert_eq!(reduce(&completed, &completion)?.state, completed);
    for transition in [
        ReducerTransition::MissionStarted,
        ReducerTransition::MissionCompleted,
        ReducerTransition::MissionFailed,
        ReducerTransition::MissionCancelled {
            reason: "again".to_owned(),
        },
        ReducerTransition::PhaseStarted,
        ReducerTransition::PhaseCompleted,
        ReducerTransition::PhaseFailed {
            error: "late".to_owned(),
        },
        ReducerTransition::PhaseSkipped {
            reason: "late".to_owned(),
        },
        ReducerTransition::PhaseRetrying,
    ] {
        let phase_id = matches!(
            transition,
            ReducerTransition::PhaseStarted
                | ReducerTransition::PhaseCompleted
                | ReducerTransition::PhaseFailed { .. }
                | ReducerTransition::PhaseSkipped { .. }
                | ReducerTransition::PhaseRetrying
        )
        .then_some("root");
        assert_eq!(
            reduce(&completed, &input(4, phase_id, transition)?),
            Err(TransitionError::MissionTerminal {
                status: MissionStatus::Completed
            })
        );
    }
    assert_eq!(completed, snapshot);
    Ok(())
}

#[test]
fn dag_validation_rejects_empty_duplicate_unknown_duplicate_edges_and_cycles() -> TestResult {
    let mission = MissionId::new("mission-1")?;
    assert_eq!(
        MissionState::new(mission.clone(), vec![]),
        Err(MissionStateBuildError::EmptyPlan)
    );
    assert_eq!(
        MissionState::new(
            mission.clone(),
            vec![definition("same", &[])?, definition("same", &[])?]
        ),
        Err(MissionStateBuildError::DuplicatePhase {
            phase_id: phase("same")?
        })
    );
    assert_eq!(
        MissionState::new(mission.clone(), vec![definition("one", &["missing"])?,]),
        Err(MissionStateBuildError::UnknownDependency {
            phase_id: phase("one")?,
            dependency: phase("missing")?
        })
    );
    assert_eq!(
        MissionState::new(
            mission.clone(),
            vec![definition("one", &[])?, definition("two", &["one", "one"])?,]
        ),
        Err(MissionStateBuildError::DuplicateDependency {
            phase_id: phase("two")?,
            dependency: phase("one")?
        })
    );
    assert!(matches!(
        MissionState::new(
            mission,
            vec![
                definition("one", &["three"])?,
                definition("two", &["one"])?,
                definition("three", &["two"])?,
            ]
        ),
        Err(MissionStateBuildError::DependencyCycle { .. })
    ));
    Ok(())
}

#[test]
fn property_style_ordered_histories_are_reproducible() -> TestResult {
    let definitions = vec![
        definition("a", &[])?,
        definition("b", &[])?,
        definition("c", &[])?,
    ];
    let initial = MissionState::new(MissionId::new("mission-1")?, definitions)?;
    let mut ordering = [0_u8, 1, 2, 3, 4, 5];
    let mut histories_checked = 0_u32;
    loop {
        let start_before_finish = (0..3).all(|phase_index| {
            let start = ordering
                .iter()
                .position(|operation| usize::from(*operation) == phase_index * 2);
            let finish = ordering
                .iter()
                .position(|operation| usize::from(*operation) == phase_index * 2 + 1);
            matches!((start, finish), (Some(start), Some(finish)) if start < finish)
        });
        if start_before_finish {
            let mut events = vec![input(1, None, ReducerTransition::MissionStarted)?];
            for (offset, operation) in ordering.iter().enumerate() {
                let phase_id = match operation / 2 {
                    0 => "a",
                    1 => "b",
                    _ => "c",
                };
                let transition = if operation % 2 == 0 {
                    ReducerTransition::PhaseStarted
                } else {
                    ReducerTransition::PhaseCompleted
                };
                events.push(input(
                    i64::try_from(offset)? + 2,
                    Some(phase_id),
                    transition,
                )?);
            }
            events.push(input(8, None, ReducerTransition::MissionCompleted)?);

            let reduce_history =
                |events: &[ReducerInput]| -> Result<MissionState, TransitionError> {
                    let mut state = initial.clone();
                    for event in events {
                        state = reduce(&state, event)?.state;
                    }
                    Ok(state)
                };
            let first = reduce_history(&events)?;
            let second = reduce_history(&events)?;
            assert_eq!(first, second);
            assert_eq!(first.status(), MissionStatus::Completed);
            histories_checked += 1;
        }
        if !next_permutation(&mut ordering) {
            break;
        }
    }
    assert_eq!(histories_checked, 90);
    Ok(())
}

fn next_permutation(values: &mut [u8]) -> bool {
    let Some(pivot) = (0..values.len().saturating_sub(1))
        .rev()
        .find(|index| values[*index] < values[*index + 1])
    else {
        return false;
    };
    let Some(successor) = (pivot + 1..values.len())
        .rev()
        .find(|index| values[*index] > values[pivot])
    else {
        return false;
    };
    values.swap(pivot, successor);
    values[pivot + 1..].reverse();
    true
}
