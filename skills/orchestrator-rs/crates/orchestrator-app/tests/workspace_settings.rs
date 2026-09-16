use orchestrator_app::{
    FixtureAdmissionPolicy, FixtureWorkspaceSeed, FreshFixtureAuthority, IsolatedFixtureRoot,
    OptionalSidecar, OverlayError, OverlayExit, RoleDenyPolicy, SettingsOverlay, SidecarState,
    WorkerFileKind, WorkerRole, WorkspaceError,
};
use orchestrator_core::{CheckpointProjection, MissionId, WorkerId, encode_current_checkpoint};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
};

static CASE: AtomicUsize = AtomicUsize::new(0);

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

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn fixture_case(label: &str) -> Result<FixtureCase, Box<dyn std::error::Error>> {
    let number = CASE.fetch_add(1, Ordering::Relaxed);
    let canonical_temp = std::fs::canonicalize(std::env::temp_dir())?;
    let parent = canonical_temp.join(format!(
        "orchestrator-rs-workspace-settings-{}-{number}-{label}",
        std::process::id()
    ));
    private_dir(&parent)?;
    let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
    let root = isolated.path().to_path_buf();
    let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
    let policy = FixtureAdmissionPolicy::new(parent.join("live-user"), checkout, &canonical_temp);
    let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
    Ok(FixtureCase {
        parent,
        root,
        authority,
    })
}

fn checkpoint(id: &str, status: &str) -> CheckpointProjection {
    CheckpointProjection {
        workspace_id: id.to_owned(),
        status: status.to_owned(),
        started_at: "2026-07-13T00:00:00Z".to_owned(),
        ..CheckpointProjection::default()
    }
}

fn workspace(
    case: &FixtureCase,
    id: &str,
) -> Result<orchestrator_app::WorkspaceAuthority, Box<dyn std::error::Error>> {
    let mission_id = MissionId::new(id)?;
    let seed = FixtureWorkspaceSeed::new(
        b"fixture mission\n".to_vec(),
        &checkpoint(id, "pending"),
        br#"{"future_plan":{"preserve":true}}"#.to_vec(),
    )?;
    Ok(case.authority.create_workspace(mission_id, seed)?)
}

fn permission_mode(path: &Path) -> Result<u32, Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(std::fs::symlink_metadata(path)?.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(0)
    }
}

#[test]
fn authority_debug_output_redacts_paths_and_user_identifiers()
-> Result<(), Box<dyn std::error::Error>> {
    const EXECUTABLE_BYTES: &[u8] = b"\x7fELFdebug-canary";
    let number = CASE.fetch_add(1, Ordering::Relaxed);
    let canonical_temp = std::fs::canonicalize(std::env::temp_dir())?;
    let parent = canonical_temp.join(format!(
        "secret-user-8675309-authority-{}-{number}",
        std::process::id()
    ));
    private_dir(&parent)?;
    let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
    let root = isolated.path().to_path_buf();
    let isolated_debug = format!("{isolated:?}");
    let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
    let policy =
        FixtureAdmissionPolicy::new(parent.join("live-user-8675309"), checkout, &canonical_temp)
            .with_expected_fixture_helper(EXECUTABLE_BYTES);
    let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
    let target = authority.create_target("secret-target-8675309")?;
    let executable =
        authority.install_fixture_executable("secret-helper-8675309", EXECUTABLE_BYTES)?;
    let mission = "secret-mission-8675309";
    let workspace = authority.create_workspace(
        MissionId::new(mission)?,
        FixtureWorkspaceSeed::new(
            b"mission".to_vec(),
            &checkpoint(mission, "pending"),
            b"{}".to_vec(),
        )?,
    )?;

    let outputs = [
        isolated_debug,
        format!("{target:?}"),
        format!("{executable:?}"),
        format!("{workspace:?}"),
    ];
    for output in &outputs {
        assert!(
            !output.contains(root.to_string_lossy().as_ref()),
            "{output}"
        );
        assert!(!output.contains("8675309"), "{output}");
        assert!(!output.contains("secret-target"), "{output}");
        assert!(!output.contains("secret-helper"), "{output}");
        assert!(!output.contains("secret-mission"), "{output}");
    }
    assert_eq!(
        outputs[0],
        "IsolatedFixtureRoot { kind: \"isolated-fixture-root\" }"
    );
    assert_eq!(
        outputs[1],
        "TargetRootAuthority { kind: \"fixture-target\" }"
    );
    assert_eq!(
        outputs[2],
        "ExecutableCapability { kind: \"fixture-executable\" }"
    );
    assert_eq!(
        outputs[3],
        "WorkspaceAuthority { kind: \"bounded-workspace\" }"
    );

    drop(workspace);
    drop(executable);
    drop(target);
    drop(authority);
    std::fs::remove_dir_all(parent)?;
    Ok(())
}

#[test]
fn creates_exact_base_layout_and_modes_from_a_fresh_root() -> Result<(), Box<dyn std::error::Error>>
{
    let case = fixture_case("layout")?;
    let _workspace = workspace(&case, "20260713-ab12cd34")?;
    let root = case.root.join("workspaces/20260713-ab12cd34");

    let expected_dirs = ["", "workers", "artifacts", "artifacts/merged", "learnings"];
    for relative in expected_dirs {
        let path = root.join(relative);
        assert!(path.is_dir(), "missing directory {relative:?}");
        assert_eq!(permission_mode(&path)?, 0o700, "directory {relative:?}");
    }
    for relative in ["mission.md", "checkpoint.json", "plan.json"] {
        let path = root.join(relative);
        assert!(path.is_file(), "missing file {relative:?}");
        assert_eq!(permission_mode(&path)?, 0o600, "file {relative:?}");
    }
    let entries = std::fs::read_dir(&root)?
        .map(|entry| entry.map(|value| value.file_name()))
        .collect::<Result<BTreeSet<_>, _>>()?;
    assert_eq!(
        entries,
        [
            "artifacts",
            "checkpoint.json",
            "learnings",
            "mission.md",
            "plan.json",
            "workers"
        ]
        .into_iter()
        .map(Into::into)
        .collect()
    );
    assert!(
        std::fs::read_dir(case.root.join("workspaces"))?.all(|entry| !entry
            .map(|value| value
                .file_name()
                .to_string_lossy()
                .starts_with(".creating-"))
            .unwrap_or(false))
    );
    Ok(())
}

#[test]
fn creates_lazy_worker_artifact_learning_and_scratch_layout()
-> Result<(), Box<dyn std::error::Error>> {
    let case = fixture_case("lazy-layout")?;
    let workspace = workspace(&case, "mission-lazy-layout")?;
    let worker = WorkerId::new("implementer-01")?;
    let phase = orchestrator_core::PhaseId::new("phase-1")?;

    workspace.prepare_phase(&worker, &phase)?;
    workspace.prepare_phase(&worker, &phase)?;
    for (kind, bytes) in [
        (WorkerFileKind::Instructions, b"instructions\n".as_slice()),
        (WorkerFileKind::Output, b"output\n".as_slice()),
        (WorkerFileKind::Context, b"context\n".as_slice()),
        (WorkerFileKind::Signal, br#"{"status":"done"}"#.as_slice()),
    ] {
        let file = workspace.worker_file(&worker, kind)?;
        file.replace(bytes)?;
        file.replace(bytes)?;
    }
    workspace
        .worker_stop_hook(&worker)?
        .replace(b"#!/bin/sh\nexit 0\n")?;
    workspace
        .learning_file(&worker)?
        .replace(br#"{"learning":"fixture"}"#)?;
    workspace
        .scratch_notes(&phase)?
        .replace(b"fixture note\n")?;

    let root = case.root.join("workspaces/mission-lazy-layout");
    for relative in [
        "workers/implementer-01",
        "workers/implementer-01/.claude",
        "workers/implementer-01/.claude/hooks",
        "artifacts/phase-1",
        "learnings",
        "scratch",
        "scratch/phase-1",
    ] {
        let path = root.join(relative);
        assert!(path.is_dir(), "missing lazy directory {relative:?}");
        assert_eq!(permission_mode(&path)?, 0o700, "directory {relative:?}");
    }
    for relative in [
        "workers/implementer-01/CLAUDE.md",
        "workers/implementer-01/output.md",
        "workers/implementer-01/workspace-context.md",
        "workers/implementer-01/orchestrator.signal.json",
        "learnings/implementer-01.json",
        "scratch/phase-1/notes.md",
    ] {
        let path = root.join(relative);
        assert!(path.is_file(), "missing lazy file {relative:?}");
        assert_eq!(permission_mode(&path)?, 0o600, "file {relative:?}");
    }
    let hook = root.join("workers/implementer-01/.claude/hooks/stop.sh");
    assert!(hook.is_file());
    assert_eq!(permission_mode(&hook)?, 0o700);
    Ok(())
}

#[cfg(unix)]
#[test]
fn lazy_file_authority_rejects_parent_and_final_symlink_swaps()
-> Result<(), Box<dyn std::error::Error>> {
    let case = fixture_case("lazy-file-nofollow")?;
    let workspace = workspace(&case, "mission-lazy-nofollow")?;
    let worker = WorkerId::new("worker-a")?;
    let file = workspace.worker_file(&worker, WorkerFileKind::Output)?;
    let outside = case.parent.join("outside-lazy");
    private_dir(&outside)?;
    private_file(&outside.join("sentinel"), b"unchanged")?;
    std::os::unix::fs::symlink(
        outside.join("sentinel"),
        case.root
            .join("workspaces/mission-lazy-nofollow/workers/worker-a/output.md"),
    )?;
    assert!(file.replace(b"must not escape").is_err());
    assert_eq!(std::fs::read(outside.join("sentinel"))?, b"unchanged");

    std::fs::remove_file(
        case.root
            .join("workspaces/mission-lazy-nofollow/workers/worker-a/output.md"),
    )?;
    let worker_path = case
        .root
        .join("workspaces/mission-lazy-nofollow/workers/worker-a");
    std::fs::rename(&worker_path, worker_path.with_extension("held"))?;
    std::os::unix::fs::symlink(&outside, &worker_path)?;
    assert!(file.replace(b"must not reach held or alias").is_err());
    assert_eq!(std::fs::read(outside.join("sentinel"))?, b"unchanged");
    Ok(())
}

#[test]
fn preserves_unknown_entries_and_classifies_optional_sidecars()
-> Result<(), Box<dyn std::error::Error>> {
    let case = fixture_case("sidecars")?;
    let workspace = workspace(&case, "mission-sidecars")?;
    let root = case.root.join("workspaces/mission-sidecars");
    let unknown = root.join("future-owner.bin");
    private_file(&unknown, b"\x00future\xff")?;

    workspace.set_sidecar(OptionalSidecar::LinearIssueId, b" \n")?;
    workspace.set_sidecar(
        OptionalSidecar::MissionPath,
        b"/definitely/missing/fixture-mission.md",
    )?;
    workspace.set_sidecar(OptionalSidecar::TargetId, b"target-a")?;
    let before = std::fs::read(&unknown)?;
    let health = workspace.inspect()?;

    assert!(health.is_degraded());
    assert!(matches!(
        health.sidecars[&OptionalSidecar::LinearIssueId],
        SidecarState::Degraded(_)
    ));
    assert!(matches!(
        health.sidecars[&OptionalSidecar::MissionPath],
        SidecarState::Degraded(_)
    ));
    assert_eq!(
        health.sidecars[&OptionalSidecar::TargetId],
        SidecarState::Present(b"target-a".to_vec())
    );
    assert_eq!(
        health.sidecars[&OptionalSidecar::PrUrl],
        SidecarState::Missing
    );
    assert_eq!(std::fs::read(&unknown)?, before);
    assert_eq!(permission_mode(&unknown)?, 0o600);
    Ok(())
}

#[test]
fn creation_failure_table_leaves_no_owned_staging_or_mutation()
-> Result<(), Box<dyn std::error::Error>> {
    let cases = ["invalid-checkpoint", "checkpoint-id-mismatch", "collision"];
    for label in cases {
        let case = fixture_case(label)?;
        let id = MissionId::new("mission-partial")?;
        let result = match label {
            "invalid-checkpoint" => FixtureWorkspaceSeed::from_encoded(
                b"mission".to_vec(),
                b"not-json".to_vec(),
                b"{}".to_vec(),
            )
            .and_then(|seed| case.authority.create_workspace(id, seed)),
            "checkpoint-id-mismatch" => {
                let seed = FixtureWorkspaceSeed::new(
                    b"mission".to_vec(),
                    &checkpoint("different-mission", "pending"),
                    b"{}".to_vec(),
                )?;
                case.authority.create_workspace(id, seed)
            }
            "collision" => {
                let first = FixtureWorkspaceSeed::new(
                    b"first".to_vec(),
                    &checkpoint("mission-partial", "pending"),
                    b"{}".to_vec(),
                )?;
                let _ = case
                    .authority
                    .create_workspace(MissionId::new("mission-partial")?, first)?;
                let second = FixtureWorkspaceSeed::new(
                    b"second".to_vec(),
                    &checkpoint("mission-partial", "pending"),
                    b"{}".to_vec(),
                )?;
                case.authority.create_workspace(id, second)
            }
            _ => unreachable!(),
        };
        assert!(result.is_err(), "{label}");
        let workspaces = case.root.join("workspaces");
        if workspaces.exists() {
            assert!(std::fs::read_dir(&workspaces)?.all(|entry| {
                !entry
                    .map(|value| {
                        value
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".creating-")
                    })
                    .unwrap_or(false)
            }));
        }
        if label == "collision" {
            assert_eq!(
                std::fs::read(workspaces.join("mission-partial/mission.md"))?,
                b"first"
            );
        }
    }
    Ok(())
}

#[test]
fn event_checkpoint_projection_preserves_history_and_recovers_idempotently()
-> Result<(), Box<dyn std::error::Error>> {
    let case = fixture_case("projection")?;
    let workspace = workspace(&case, "mission-projection")?;
    let root = case.root.join("workspaces/mission-projection");
    let history = b"not-json-but-preserved\n{\"type\":\"future\",\"unknown\":1}\n";
    private_file(&root.join("events.jsonl"), history)?;
    let mut writer = workspace.into_fixture_projection_writer()?;
    let event = br#"{"id":"evt-1","type":"mission.started","timestamp":"2026-07-13T00:00:01Z","sequence":1,"mission_id":"mission-projection","future":true}"#;
    let next = encode_current_checkpoint(&checkpoint("mission-projection", "in_progress"))?;

    writer.commit_event_checkpoint(event, &next)?;
    assert_eq!(
        std::fs::read(root.join("events.jsonl"))?,
        [history.as_slice(), event.as_slice(), b"\n"].concat()
    );
    assert_eq!(std::fs::read(root.join("checkpoint.json"))?, next);

    // Simulate a crash after the event rename but before the checkpoint rename.
    let event2 = br#"{"id":"evt-2","type":"mission.completed","timestamp":"2026-07-13T00:00:02Z","sequence":2,"mission_id":"mission-projection","future":2}"#;
    let next2 = encode_current_checkpoint(&checkpoint("mission-projection", "completed"))?;
    let mut partial_log = std::fs::read(root.join("events.jsonl"))?;
    partial_log.extend_from_slice(event2);
    partial_log.push(b'\n');
    private_file(&root.join("events.jsonl"), &partial_log)?;
    private_file(&root.join(".checkpoint.json.projection-tmp"), &next2)?;

    writer.commit_event_checkpoint(event2, &next2)?;
    assert_eq!(std::fs::read(root.join("events.jsonl"))?, partial_log);
    assert_eq!(std::fs::read(root.join("checkpoint.json"))?, next2);
    assert!(!root.join(".checkpoint.json.projection-tmp").exists());
    Ok(())
}

#[test]
fn projection_preserves_unknown_transaction_files_on_conflict()
-> Result<(), Box<dyn std::error::Error>> {
    let case = fixture_case("projection-conflict")?;
    let workspace = workspace(&case, "mission-conflict")?;
    let root = case.root.join("workspaces/mission-conflict");
    let mut writer = workspace.into_fixture_projection_writer()?;
    private_file(&root.join(".events.jsonl.projection-tmp"), b"unknown-owner")?;
    let next = encode_current_checkpoint(&checkpoint("mission-conflict", "in_progress"))?;
    let event = br#"{"id":"evt-conflict","type":"mission.started","timestamp":"2026-07-13T00:00:01Z","sequence":1,"mission_id":"mission-conflict"}"#;
    let result = writer.commit_event_checkpoint(event, &next);
    assert!(matches!(result, Err(WorkspaceError::ProjectionConflict)));
    assert_eq!(
        std::fs::read(root.join(".events.jsonl.projection-tmp"))?,
        b"unknown-owner"
    );
    Ok(())
}

#[test]
fn projection_writer_rejects_invalid_multiple_and_oversized_events()
-> Result<(), Box<dyn std::error::Error>> {
    let case = fixture_case("projection-invalid-event")?;
    let workspace = workspace(&case, "mission-invalid-event")?;
    let root = case.root.join("workspaces/mission-invalid-event");
    let initial_checkpoint = std::fs::read(root.join("checkpoint.json"))?;
    let mut writer = workspace.into_fixture_projection_writer()?;
    let running = encode_current_checkpoint(&checkpoint("mission-invalid-event", "in_progress"))?;

    for invalid in [
        b"not-json".as_slice(),
        b"{}\n{}".as_slice(),
        b"{}\r".as_slice(),
        b"".as_slice(),
    ] {
        assert!(matches!(
            writer.commit_event_checkpoint(invalid, &running),
            Err(WorkspaceError::InvalidFixtureEvent)
        ));
    }
    let oversized = vec![b'x'; 1024 * 1024 + 1];
    assert!(matches!(
        writer.commit_event_checkpoint(&oversized, &running),
        Err(WorkspaceError::InvalidFixtureEvent)
    ));
    assert!(!root.join("events.jsonl").exists());
    assert_eq!(
        std::fs::read(root.join("checkpoint.json"))?,
        initial_checkpoint
    );
    Ok(())
}

#[test]
fn projection_writer_rejects_duplicate_ids_sequences_and_conflicting_prior_history()
-> Result<(), Box<dyn std::error::Error>> {
    let case = fixture_case("projection-exact-prior")?;
    let workspace = workspace(&case, "mission-exact-prior")?;
    let root = case.root.join("workspaces/mission-exact-prior");
    let mut writer = workspace.into_fixture_projection_writer()?;
    let started = br#"{"id":"evt-1","type":"mission.started","timestamp":"2026-07-13T00:00:01Z","sequence":1,"mission_id":"mission-exact-prior"}"#;
    let running = encode_current_checkpoint(&checkpoint("mission-exact-prior", "in_progress"))?;
    writer.commit_event_checkpoint(started, &running)?;
    let acknowledged_log = std::fs::read(root.join("events.jsonl"))?;
    let acknowledged_checkpoint = std::fs::read(root.join("checkpoint.json"))?;

    let duplicate_id = br#"{"id":"evt-1","type":"mission.completed","timestamp":"2026-07-13T00:00:02Z","sequence":2,"mission_id":"mission-exact-prior"}"#;
    let completed = encode_current_checkpoint(&checkpoint("mission-exact-prior", "completed"))?;
    assert!(matches!(
        writer.commit_event_checkpoint(duplicate_id, &completed),
        Err(WorkspaceError::ProjectionConflict)
    ));
    let duplicate_sequence = br#"{"id":"evt-2","type":"mission.started","timestamp":"2026-07-13T00:00:02Z","sequence":1,"mission_id":"mission-exact-prior"}"#;
    assert!(matches!(
        writer.commit_event_checkpoint(duplicate_sequence, &running),
        Err(WorkspaceError::ProjectionConflict)
    ));
    assert_eq!(std::fs::read(root.join("events.jsonl"))?, acknowledged_log);
    assert_eq!(
        std::fs::read(root.join("checkpoint.json"))?,
        acknowledged_checkpoint
    );

    let mut conflicting_history = acknowledged_log;
    conflicting_history.extend_from_slice(
        br#"{"id":"external","type":"future.event","timestamp":"2026-07-13T00:00:09Z","sequence":9,"mission_id":"mission-exact-prior"}"#,
    );
    conflicting_history.push(b'\n');
    private_file(&root.join("events.jsonl"), &conflicting_history)?;
    assert!(matches!(
        writer.commit_event_checkpoint(duplicate_id, &completed),
        Err(WorkspaceError::ProjectionConflict)
    ));
    assert_eq!(
        std::fs::read(root.join("events.jsonl"))?,
        conflicting_history
    );
    Ok(())
}

#[test]
fn role_policy_and_all_exit_modes_restore_byte_exact_backup()
-> Result<(), Box<dyn std::error::Error>> {
    let roles = [
        (WorkerRole::Planner, true),
        (WorkerRole::Implementer, false),
        (WorkerRole::Reviewer, true),
    ];
    for (index, exit) in [
        OverlayExit::Success,
        OverlayExit::Failure,
        OverlayExit::Cancelled,
        OverlayExit::TimedOut,
        OverlayExit::Unwind,
    ]
    .into_iter()
    .enumerate()
    {
        for (role, denies_edit) in roles {
            let case = fixture_case(&format!("overlay-{index}-{role:?}"))?;
            let workspace = workspace(&case, "mission-overlay")?;
            let target = case.authority.create_target("target-a")?;
            let claude = case.root.join("targets/target-a/.claude");
            private_dir(&claude)?;
            let original = [b"\x00\xffnot-json\n".as_slice(), &[index as u8]].concat();
            private_file(&claude.join("settings.local.json"), &original)?;
            let worker = WorkerId::new("worker-a")?;

            let overlay = SettingsOverlay::install(
                target,
                &workspace,
                &worker,
                RoleDenyPolicy::for_role(role),
            )?;
            assert_eq!(
                std::fs::read(claude.join("settings.local.json.orchestrator-backup"))?,
                original
            );
            assert_eq!(
                permission_mode(&claude.join("settings.local.json.orchestrator-backup"))?,
                0o600
            );
            let installed: serde_json::Value =
                serde_json::from_slice(&std::fs::read(claude.join("settings.local.json"))?)?;
            let deny = installed["permissions"]["deny"]
                .as_array()
                .ok_or("deny is not an array")?;
            assert_eq!(deny.iter().any(|value| value == "Edit"), denies_edit);
            overlay.finish(exit)?;

            assert_eq!(std::fs::read(claude.join("settings.local.json"))?, original);
            assert!(
                !claude
                    .join("settings.local.json.orchestrator-backup")
                    .exists()
            );
            assert!(!case
                .root
                .join("workspaces/mission-overlay/workers/worker-a/.orchestrator-settings-overlay.json")
                .exists());
        }
    }
    Ok(())
}

#[test]
fn settings_overlay_rejects_a_different_fixture_before_worker_mutation()
-> Result<(), Box<dyn std::error::Error>> {
    let workspace_case = fixture_case("overlay-workspace-boundary")?;
    let target_case = fixture_case("overlay-target-boundary")?;
    let workspace = workspace(&workspace_case, "mission-boundary-mismatch")?;
    let target = target_case.authority.create_target("target-a")?;
    let worker = WorkerId::new("worker-a")?;

    let result = SettingsOverlay::install(
        target,
        &workspace,
        &worker,
        RoleDenyPolicy::for_role(WorkerRole::Implementer),
    );

    assert!(matches!(
        result,
        Err(OverlayError::InvalidRecord(ref message))
            if message == "target and workspace belong to different fixture authorities"
    ));
    let recovery = target_case
        .authority
        .recover_settings_overlay(&workspace, &worker);
    assert!(matches!(
        recovery,
        Err(OverlayError::InvalidRecord(ref message))
            if message == "workspace belongs to a different fixture authority"
    ));
    assert!(
        !workspace_case
            .root
            .join("workspaces/mission-boundary-mismatch/workers/worker-a")
            .exists(),
        "a rejected cross-fixture overlay created worker state"
    );
    assert!(
        !target_case.root.join("targets/target-a/.claude").exists(),
        "a rejected cross-fixture overlay or recovery mutated its target"
    );
    Ok(())
}

#[test]
fn stale_workspace_error_precedes_cross_fixture_mismatch_without_mutation()
-> Result<(), Box<dyn std::error::Error>> {
    let workspace_case = fixture_case("overlay-stale-workspace")?;
    let target_case = fixture_case("overlay-stale-target")?;
    let workspace = workspace(&workspace_case, "mission-stale-boundary")?;
    let workspace_path = workspace_case
        .root
        .join("workspaces/mission-stale-boundary");
    std::fs::rename(
        &workspace_path,
        workspace_case.root.join("displaced-stale-workspace"),
    )?;
    private_dir(&workspace_path)?;
    let target = target_case.authority.create_target("target-a")?;
    let worker = WorkerId::new("worker-a")?;

    let result = SettingsOverlay::install(
        target,
        &workspace,
        &worker,
        RoleDenyPolicy::for_role(WorkerRole::Implementer),
    );

    assert!(matches!(
        result,
        Err(OverlayError::Workspace(WorkspaceError::IdentityChanged))
    ));
    assert!(
        !workspace_path.join("workers/worker-a").exists(),
        "a stale workspace created worker state before rejection"
    );
    assert!(
        !target_case.root.join("targets/target-a/.claude").exists(),
        "a stale cross-fixture workspace mutated its target before rejection"
    );
    Ok(())
}

#[test]
fn absent_original_drop_and_unwind_remove_only_owned_overlay()
-> Result<(), Box<dyn std::error::Error>> {
    for label in ["drop", "unwind"] {
        let case = fixture_case(label)?;
        let workspace = workspace(&case, "mission-absent")?;
        let target = case.authority.create_target("target-a")?;
        let worker = WorkerId::new("worker-a")?;
        if label == "drop" {
            let overlay = SettingsOverlay::install(
                target,
                &workspace,
                &worker,
                RoleDenyPolicy::for_role(WorkerRole::Implementer),
            )?;
            drop(overlay);
        } else {
            let overlay = SettingsOverlay::install(
                target,
                &workspace,
                &worker,
                RoleDenyPolicy::for_role(WorkerRole::Implementer),
            )?;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _overlay = overlay;
                std::panic::resume_unwind(Box::new("injected unwind"));
            }));
            assert!(result.is_err());
        }
        assert!(!case.root.join("targets/target-a/.claude").exists());
    }
    Ok(())
}

#[test]
fn concurrent_teardown_and_repeated_recovery_are_idempotent()
-> Result<(), Box<dyn std::error::Error>> {
    let case = fixture_case("concurrent")?;
    let workspace = workspace(&case, "mission-concurrent")?;
    let target = case.authority.create_target("target-a")?;
    let claude = case.root.join("targets/target-a/.claude");
    private_dir(&claude)?;
    let original = b"exact original\x00\xff";
    private_file(&claude.join("settings.local.json"), original)?;
    let overlay = SettingsOverlay::install(
        target,
        &workspace,
        &WorkerId::new("worker-a")?,
        RoleDenyPolicy::for_role(WorkerRole::Reviewer),
    )?;
    let barrier = Arc::new(Barrier::new(5));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let handle = overlay.clone();
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            handle.restore().map_err(|error| error.to_string())
        }));
    }
    barrier.wait();
    for thread in threads {
        thread.join().map_err(|_| "teardown thread panicked")??;
    }
    overlay.restore()?;
    overlay.restore()?;
    assert_eq!(std::fs::read(claude.join("settings.local.json"))?, original);
    assert!(
        !claude
            .join("settings.local.json.orchestrator-backup")
            .exists()
    );
    Ok(())
}

#[test]
fn durable_recovery_repairs_partial_restore_states_idempotently()
-> Result<(), Box<dyn std::error::Error>> {
    for state in ["missing-backup", "original-plus-backup", "absent-original"] {
        let case = fixture_case(state)?;
        let workspace = workspace(&case, "mission-recovery")?;
        let target = case.authority.create_target("target-a")?;
        let claude = case.root.join("targets/target-a/.claude");
        if state != "absent-original" {
            private_dir(&claude)?;
            private_file(
                &claude.join("settings.local.json"),
                b"\x00original\xffbytes",
            )?;
        }
        let worker = WorkerId::new("worker-a")?;
        let overlay = SettingsOverlay::install(
            target,
            &workspace,
            &worker,
            RoleDenyPolicy::for_role(WorkerRole::Implementer),
        )?;
        overlay.abandon_for_recovery()?;

        match state {
            "missing-backup" => {
                std::fs::remove_file(claude.join("settings.local.json.orchestrator-backup"))?;
            }
            "original-plus-backup" => {
                private_file(
                    &claude.join("settings.local.json"),
                    b"\x00original\xffbytes",
                )?;
                let record_path = case.root.join(
                    "workspaces/mission-recovery/workers/worker-a/.orchestrator-settings-overlay.json",
                );
                let mut record: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&record_path)?)?;
                record["state"] = serde_json::Value::String("backed_up".to_owned());
                private_file(&record_path, &serde_json::to_vec(&record)?)?;
            }
            "absent-original" => {}
            _ => unreachable!(),
        }

        assert!(
            case.authority
                .recover_settings_overlay(&workspace, &worker)?
        );
        assert!(
            !case
                .authority
                .recover_settings_overlay(&workspace, &worker)?
        );
        if state == "absent-original" {
            assert!(!case.root.join("targets/target-a/.claude").exists());
        } else {
            assert_eq!(
                std::fs::read(claude.join("settings.local.json"))?,
                b"\x00original\xffbytes"
            );
            assert!(
                !claude
                    .join("settings.local.json.orchestrator-backup")
                    .exists()
            );
        }
    }
    Ok(())
}

#[test]
fn restart_recovery_is_exclusive_and_restores_after_original_authority_drops()
-> Result<(), Box<dyn std::error::Error>> {
    let canonical_temp = std::fs::canonicalize(std::env::temp_dir())?;
    let parent = canonical_temp.join(format!(
        "orchestrator-rs-restart-recovery-{}",
        std::process::id()
    ));
    private_dir(&parent)?;
    let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
    let root = isolated.path().to_path_buf();
    let policy = FixtureAdmissionPolicy::new(
        parent.join("live-user"),
        std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?,
        &canonical_temp,
    );
    let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
    let seed = FixtureWorkspaceSeed::new(
        b"mission".to_vec(),
        &checkpoint("mission-restart", "pending"),
        b"{}".to_vec(),
    )?;
    let workspace = authority.create_workspace(MissionId::new("mission-restart")?, seed)?;
    let target = authority.create_target("target-a")?;
    let claude = root.join("targets/target-a/.claude");
    private_dir(&claude)?;
    private_file(&claude.join("settings.local.json"), b"restart-original")?;
    let worker = WorkerId::new("worker-a")?;
    SettingsOverlay::install(
        target,
        &workspace,
        &worker,
        RoleDenyPolicy::for_role(WorkerRole::Reviewer),
    )?
    .abandon_for_recovery()?;

    assert!(
        FreshFixtureAuthority::recover(IsolatedFixtureRoot::identify(&root)?, &policy).is_err()
    );
    drop(workspace);
    drop(authority);
    let recovered = FreshFixtureAuthority::recover(IsolatedFixtureRoot::identify(&root)?, &policy)?;
    let recovered_workspace = recovered.open_workspace(MissionId::new("mission-restart")?)?;
    assert!(recovered.recover_settings_overlay(&recovered_workspace, &worker)?);
    assert_eq!(
        std::fs::read(claude.join("settings.local.json"))?,
        b"restart-original"
    );
    drop(recovered_workspace);
    drop(recovered);
    std::fs::remove_dir_all(parent)?;
    Ok(())
}

#[test]
fn restart_recovery_rejects_target_identity_substitution() -> Result<(), Box<dyn std::error::Error>>
{
    let canonical_temp = std::fs::canonicalize(std::env::temp_dir())?;
    let parent = canonical_temp.join(format!(
        "orchestrator-rs-target-substitution-{}",
        std::process::id()
    ));
    private_dir(&parent)?;
    let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
    let root = isolated.path().to_path_buf();
    let policy = FixtureAdmissionPolicy::new(
        parent.join("live-user"),
        std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?,
        &canonical_temp,
    );
    let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
    let seed = FixtureWorkspaceSeed::new(
        b"mission".to_vec(),
        &checkpoint("mission-substitution", "pending"),
        b"{}".to_vec(),
    )?;
    let workspace = authority.create_workspace(MissionId::new("mission-substitution")?, seed)?;
    let target = authority.create_target("target-a")?;
    let worker = WorkerId::new("worker-a")?;
    SettingsOverlay::install(
        target,
        &workspace,
        &worker,
        RoleDenyPolicy::for_role(WorkerRole::Implementer),
    )?
    .abandon_for_recovery()?;
    drop(workspace);
    drop(authority);
    std::fs::rename(
        root.join("targets/target-a"),
        root.join("targets/original-target"),
    )?;
    private_dir(&root.join("targets/target-a"))?;

    let recovered = FreshFixtureAuthority::recover(IsolatedFixtureRoot::identify(&root)?, &policy)?;
    let recovered_workspace = recovered.open_workspace(MissionId::new("mission-substitution")?)?;
    assert!(
        recovered
            .recover_settings_overlay(&recovered_workspace, &worker)
            .is_err()
    );
    assert!(!root.join("targets/target-a/.claude").exists());
    assert!(
        root.join(
            "workspaces/mission-substitution/workers/worker-a/.orchestrator-settings-overlay.json"
        )
        .exists()
    );
    drop(recovered_workspace);
    drop(recovered);
    std::fs::remove_dir_all(parent)?;
    Ok(())
}

#[test]
fn concurrent_unknown_edit_is_preserved_and_forces_recovery_required()
-> Result<(), Box<dyn std::error::Error>> {
    let case = fixture_case("unknown-edit")?;
    let workspace = workspace(&case, "mission-edit")?;
    let target = case.authority.create_target("target-a")?;
    let claude = case.root.join("targets/target-a/.claude");
    private_dir(&claude)?;
    let original = b"original";
    private_file(&claude.join("settings.local.json"), original)?;
    let overlay = SettingsOverlay::install(
        target,
        &workspace,
        &WorkerId::new("worker-a")?,
        RoleDenyPolicy::for_role(WorkerRole::Implementer),
    )?;
    private_file(
        &claude.join("settings.local.json"),
        b"user concurrent bytes",
    )?;

    assert!(overlay.restore().is_err());
    assert_eq!(
        std::fs::read(claude.join("settings.local.json"))?,
        b"user concurrent bytes"
    );
    assert_eq!(
        std::fs::read(claude.join("settings.local.json.orchestrator-backup"))?,
        original
    );
    assert!(
        case.root
            .join("workspaces/mission-edit/workers/worker-a/.orchestrator-settings-overlay.json")
            .exists()
    );
    Ok(())
}

#[test]
fn failed_restore_retains_target_lease_and_recovery_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let case = fixture_case("failed-restore-lease")?;
    let workspace = workspace(&case, "mission-lease")?;
    let target = case.authority.create_target("target-a")?;
    let worker = WorkerId::new("worker-a")?;
    let overlay = SettingsOverlay::install(
        target,
        &workspace,
        &worker,
        RoleDenyPolicy::for_role(WorkerRole::Implementer),
    )?;
    let settings = case
        .root
        .join("targets/target-a/.claude/settings.local.json");
    private_file(&settings, b"concurrent user bytes")?;

    assert!(overlay.restore().is_err());
    drop(overlay);
    let recovery = case
        .root
        .join("workspaces/mission-lease/workers/worker-a/.orchestrator-settings-overlay.json");
    assert!(recovery.is_file());
    assert_eq!(std::fs::read(&settings)?, b"concurrent user bytes");

    let second_target = case.authority.open_target("target-a")?;
    assert!(
        SettingsOverlay::install(
            second_target,
            &workspace,
            &WorkerId::new("worker-b")?,
            RoleDenyPolicy::for_role(WorkerRole::Reviewer),
        )
        .is_err()
    );
    assert!(recovery.is_file());
    assert_eq!(std::fs::read(&settings)?, b"concurrent user bytes");
    Ok(())
}

#[cfg(unix)]
#[test]
fn nofollow_denies_final_component_and_target_or_workspace_alias_swaps()
-> Result<(), Box<dyn std::error::Error>> {
    let case = fixture_case("nofollow")?;
    let workspace = workspace(&case, "mission-nofollow")?;
    let target = case.authority.create_target("target-a")?;
    let outside = case.parent.join("outside");
    private_dir(&outside)?;
    private_file(&outside.join("sentinel"), b"unchanged")?;
    let claude = case.root.join("targets/target-a/.claude");
    private_dir(&claude)?;
    std::os::unix::fs::symlink(outside.join("sentinel"), claude.join("settings.local.json"))?;
    assert!(
        SettingsOverlay::install(
            target,
            &workspace,
            &WorkerId::new("worker-a")?,
            RoleDenyPolicy::for_role(WorkerRole::Implementer),
        )
        .is_err()
    );
    assert_eq!(std::fs::read(outside.join("sentinel"))?, b"unchanged");

    let target = case.authority.create_target("target-b")?;
    std::fs::rename(
        case.root.join("targets/target-b"),
        case.root.join("targets/target-b-original"),
    )?;
    std::os::unix::fs::symlink(&outside, case.root.join("targets/target-b"))?;
    assert!(
        SettingsOverlay::install(
            target,
            &workspace,
            &WorkerId::new("worker-b")?,
            RoleDenyPolicy::for_role(WorkerRole::Planner),
        )
        .is_err()
    );
    assert!(!outside.join(".claude").exists());

    let workspace_root = case.root.join("workspaces/mission-nofollow");
    std::fs::rename(
        &workspace_root,
        case.root.join("workspaces/original-workspace"),
    )?;
    std::os::unix::fs::symlink(&outside, &workspace_root)?;
    assert!(workspace.inspect().is_err());
    assert_eq!(std::fs::read(outside.join("sentinel"))?, b"unchanged");
    Ok(())
}

#[cfg(unix)]
#[test]
fn nofollow_restore_rejects_claude_directory_swap_without_touching_alias_target()
-> Result<(), Box<dyn std::error::Error>> {
    let case = fixture_case("claude-swap")?;
    let workspace = workspace(&case, "mission-swap")?;
    let target = case.authority.create_target("target-a")?;
    let claude = case.root.join("targets/target-a/.claude");
    private_dir(&claude)?;
    private_file(&claude.join("settings.local.json"), b"original")?;
    let overlay = SettingsOverlay::install(
        target,
        &workspace,
        &WorkerId::new("worker-a")?,
        RoleDenyPolicy::for_role(WorkerRole::Reviewer),
    )?;
    let held = case.root.join("targets/target-a/.claude-held");
    std::fs::rename(&claude, &held)?;
    let outside = case.parent.join("outside-claude");
    private_dir(&outside)?;
    private_file(&outside.join("sentinel"), b"unchanged")?;
    std::os::unix::fs::symlink(&outside, &claude)?;

    assert!(overlay.restore().is_err());
    assert_eq!(std::fs::read(outside.join("sentinel"))?, b"unchanged");
    assert!(!outside.join("settings.local.json").exists());
    assert!(
        held.join("settings.local.json.orchestrator-backup")
            .exists()
    );
    assert!(
        case.root
            .join("workspaces/mission-swap/workers/worker-a/.orchestrator-settings-overlay.json")
            .exists()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn fixture_root_relocation_or_alias_swap_revokes_derived_authority()
-> Result<(), Box<dyn std::error::Error>> {
    let case = fixture_case("root-relocation")?;
    let workspace = workspace(&case, "mission-root-swap")?;
    let relocated = case.parent.join("relocated-fixture");
    std::fs::rename(&case.root, &relocated)?;
    let outside = case.parent.join("outside-root-alias");
    private_dir(&outside)?;
    private_file(&outside.join("sentinel"), b"unchanged")?;
    std::os::unix::fs::symlink(&outside, &case.root)?;

    assert!(workspace.inspect().is_err());
    assert_eq!(std::fs::read(outside.join("sentinel"))?, b"unchanged");
    assert!(!outside.join("workspaces").exists());
    Ok(())
}

#[test]
fn admission_rejects_modes_nonfresh_roots_and_live_home_overlap()
-> Result<(), Box<dyn std::error::Error>> {
    let canonical_temp = std::fs::canonicalize(std::env::temp_dir())?;
    for (index, kind) in ["mode", "nonfresh", "live-home"].into_iter().enumerate() {
        let parent = canonical_temp.join(format!(
            "orchestrator-rs-admission-{}-{index}",
            std::process::id()
        ));
        let live_user = parent.join("user");
        let fixture_parent = if kind == "live-home" {
            live_user.join(".alluka")
        } else {
            parent.join("fixture-parent")
        };
        private_dir(&fixture_parent)?;
        let isolated = IsolatedFixtureRoot::create_fresh(&fixture_parent)?;
        let root = isolated.path().to_path_buf();
        if kind == "mode" {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755))?;
            }
        } else if kind == "nonfresh" {
            private_file(&root.join("seed"), b"not declared")?;
        }
        let policy = FixtureAdmissionPolicy::new(
            &live_user,
            std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?,
            &canonical_temp,
        );
        assert!(
            FreshFixtureAuthority::admit(isolated, &policy).is_err(),
            "{kind}"
        );
        let _ = std::fs::remove_dir_all(parent);
    }
    Ok(())
}

#[test]
fn admitted_fixture_operations_leave_live_home_sentinels_byte_exact()
-> Result<(), Box<dyn std::error::Error>> {
    let canonical_temp = std::fs::canonicalize(std::env::temp_dir())?;
    let parent = canonical_temp.join(format!(
        "orchestrator-rs-live-sentinels-{}",
        std::process::id()
    ));
    let live_user = parent.join("live-user");
    let alluka = live_user.join(".alluka");
    let via = live_user.join(".via");
    private_dir(&alluka)?;
    private_dir(&via)?;
    let alluka_bytes = b"live alluka \x00\xff";
    let via_bytes = b"live via \xff\x00";
    private_file(&alluka.join("sentinel"), alluka_bytes)?;
    private_file(&via.join("sentinel"), via_bytes)?;
    let fixture_parent = parent.join("fixture-parent");
    private_dir(&fixture_parent)?;
    let isolated = IsolatedFixtureRoot::create_fresh(&fixture_parent)?;
    let policy = FixtureAdmissionPolicy::new(
        &live_user,
        std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?,
        &canonical_temp,
    );
    let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
    let seed = FixtureWorkspaceSeed::new(
        b"mission".to_vec(),
        &checkpoint("mission-sentinel", "pending"),
        b"{}".to_vec(),
    )?;
    let workspace = authority.create_workspace(MissionId::new("mission-sentinel")?, seed)?;
    let target = authority.create_target("target-a")?;
    let overlay = SettingsOverlay::install(
        target,
        &workspace,
        &WorkerId::new("worker-a")?,
        RoleDenyPolicy::for_role(WorkerRole::Planner),
    )?;
    overlay.finish(OverlayExit::TimedOut)?;

    assert_eq!(std::fs::read(alluka.join("sentinel"))?, alluka_bytes);
    assert_eq!(std::fs::read(via.join("sentinel"))?, via_bytes);
    assert_eq!(permission_mode(&alluka.join("sentinel"))?, 0o600);
    assert_eq!(permission_mode(&via.join("sentinel"))?, 0o600);
    assert_eq!(std::fs::read_dir(&alluka)?.count(), 1);
    assert_eq!(std::fs::read_dir(&via)?.count(), 1);
    std::fs::remove_dir_all(parent)?;
    Ok(())
}
