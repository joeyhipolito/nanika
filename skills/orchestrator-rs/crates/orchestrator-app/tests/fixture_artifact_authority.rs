use orchestrator_app::{
    FixtureAdmissionPolicy, FixtureArtifactErrorKind, FixtureWorkspaceSeed, FreshFixtureAuthority,
    IsolatedFixtureRoot,
};
use orchestrator_core::{CheckpointProjection, MissionId, PhaseId, WorkerId};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const EXPECTED: &[u8] = b"fixture output\n";
const ARTIFACT_NAME: &str = "output.md";

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
    let nonce = CASE.fetch_add(1, Ordering::Relaxed);
    let temporary = std::fs::canonicalize(std::env::temp_dir())?;
    let parent = temporary.join(format!(
        "orchestrator-rs-artifact-authority-{}-{nonce}-{label}",
        std::process::id()
    ));
    private_dir(&parent)?;
    let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
    let root = isolated.path().to_path_buf();
    let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
    let policy = FixtureAdmissionPolicy::new(parent.join("live-user"), checkout, &temporary);
    let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
    Ok(FixtureCase {
        parent,
        root,
        authority,
    })
}

fn checkpoint(id: &str) -> CheckpointProjection {
    CheckpointProjection {
        workspace_id: id.to_owned(),
        status: "pending".to_owned(),
        started_at: "2026-07-15T00:00:00Z".to_owned(),
        ..CheckpointProjection::default()
    }
}

fn workspace(
    case: &FixtureCase,
    id: &str,
) -> Result<orchestrator_app::WorkspaceAuthority, Box<dyn std::error::Error>> {
    Ok(case.authority.create_workspace(
        MissionId::new(id)?,
        FixtureWorkspaceSeed::new(b"fixture\n".to_vec(), &checkpoint(id), b"{}".to_vec())?,
    )?)
}

fn artifact_path(case: &FixtureCase, mission: &str, phase: &str, attempt: u32) -> PathBuf {
    case.root
        .join("workspaces")
        .join(mission)
        .join("artifacts")
        .join(phase)
        .join(format!("attempt-{attempt}"))
        .join(ARTIFACT_NAME)
}

#[test]
fn authority_survives_workspace_consumption_and_attests_exact_publication() -> TestResult {
    let case = fixture_case("consume")?;
    let mission = MissionId::new("mission-artifact-consume")?;
    let phase = PhaseId::new("phase-1")?;
    let workspace = workspace(&case, mission.as_str())?;
    let publisher = workspace.bind_fixture_artifact(&phase, 1, ARTIFACT_NAME, EXPECTED)?;
    let _projection_writer = workspace.into_fixture_projection_writer()?;

    let attestor = publisher.publish()?;
    let receipt = attestor.attest()?;

    assert_eq!(receipt.mission_id(), &mission);
    assert_eq!(receipt.phase_id(), &phase);
    assert_eq!(receipt.attempt(), 1);
    assert_eq!(
        receipt.relative_path(),
        Path::new("artifacts/phase-1/attempt-1/output.md")
    );
    assert_eq!(
        receipt.digest(),
        "fixture-fnv1a64-v1:b8a597e96207d04f:000000000000000f"
    );
    assert_eq!(receipt.bytes(), EXPECTED.len() as u64);
    assert_eq!(
        std::fs::read(artifact_path(&case, mission.as_str(), phase.as_str(), 1))?,
        EXPECTED
    );
    Ok(())
}

#[test]
fn zero_attempt_is_rejected_before_artifact_namespace_creation() -> TestResult {
    let case = fixture_case("zero-attempt")?;
    let mission = "mission-artifact-zero-attempt";
    let phase = PhaseId::new("phase-1")?;
    let workspace = workspace(&case, mission)?;

    let error = match workspace.bind_fixture_artifact(&phase, 0, ARTIFACT_NAME, EXPECTED) {
        Err(error) => error,
        Ok(_) => return Err("zero attempt was admitted".into()),
    };

    assert_eq!(error.kind(), FixtureArtifactErrorKind::InvalidAttempt);
    assert!(!artifact_path(&case, mission, phase.as_str(), 0).exists());
    Ok(())
}

#[test]
fn second_absent_authority_cannot_clobber_first_publication() -> TestResult {
    let case = fixture_case("no-clobber")?;
    let mission = "mission-artifact-no-clobber";
    let phase = PhaseId::new("phase-1")?;
    let workspace = workspace(&case, mission)?;
    let first = workspace.bind_fixture_artifact(&phase, 1, ARTIFACT_NAME, EXPECTED)?;
    let second = workspace.bind_fixture_artifact(&phase, 1, ARTIFACT_NAME, EXPECTED)?;

    let first_receipt = first.publish()?.attest()?;
    let error = match second.publish() {
        Err(error) => error,
        Ok(_) => return Err("second publisher unexpectedly replaced the publication".into()),
    };

    assert_eq!(error.kind(), FixtureArtifactErrorKind::Conflict);
    assert_eq!(
        std::fs::read(artifact_path(&case, mission, phase.as_str(), 1))?,
        EXPECTED
    );
    assert_eq!(first_receipt.bytes(), EXPECTED.len() as u64);
    Ok(())
}

#[test]
fn exact_existing_artifact_is_readmitted_with_the_same_file_identity() -> TestResult {
    let case = fixture_case("readmit")?;
    let mission = "mission-artifact-readmit";
    let phase = PhaseId::new("phase-1")?;
    let workspace = workspace(&case, mission)?;
    let first = workspace.bind_fixture_artifact(&phase, 1, ARTIFACT_NAME, EXPECTED)?;
    let first_receipt = first.publish()?.attest()?;

    let recovered_workspace = case.authority.open_workspace(MissionId::new(mission)?)?;
    let recovered =
        recovered_workspace.bind_fixture_artifact(&phase, 1, ARTIFACT_NAME, EXPECTED)?;
    let recovered_receipt = recovered.publish()?.attest()?;

    assert_eq!(
        recovered_receipt.file_identity(),
        first_receipt.file_identity()
    );
    assert_eq!(recovered_receipt.digest(), first_receipt.digest());
    Ok(())
}

#[test]
fn later_attempt_publishes_in_a_distinct_namespace_and_inode() -> TestResult {
    let case = fixture_case("distinct-attempt")?;
    let mission = "mission-artifact-distinct-attempt";
    let phase = PhaseId::new("phase-1")?;
    let workspace = workspace(&case, mission)?;
    let first = workspace.bind_fixture_artifact(&phase, 1, ARTIFACT_NAME, EXPECTED)?;
    let first_receipt = first.publish()?.attest()?;

    let recovered_workspace = case.authority.open_workspace(MissionId::new(mission)?)?;
    let second = recovered_workspace.bind_fixture_artifact(&phase, 2, ARTIFACT_NAME, EXPECTED)?;
    let second_receipt = second.publish()?.attest()?;

    assert_eq!(
        first_receipt.relative_path(),
        Path::new("artifacts/phase-1/attempt-1/output.md")
    );
    assert_eq!(
        second_receipt.relative_path(),
        Path::new("artifacts/phase-1/attempt-2/output.md")
    );
    assert_ne!(
        second_receipt.file_identity(),
        first_receipt.file_identity()
    );
    assert_eq!(
        std::fs::read(artifact_path(&case, mission, phase.as_str(), 1))?,
        EXPECTED
    );
    assert_eq!(
        std::fs::read(artifact_path(&case, mission, phase.as_str(), 2))?,
        EXPECTED
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlink_publication_name_is_rejected_without_touching_its_target() -> TestResult {
    use std::os::unix::fs::symlink;

    let case = fixture_case("symlink")?;
    let mission = "mission-artifact-symlink";
    let phase = PhaseId::new("phase-1")?;
    let workspace = workspace(&case, mission)?;
    workspace.prepare_phase(&WorkerId::new("worker-1")?, &phase)?;
    let target = case.parent.join("outside-secret");
    private_file(&target, b"outside\n")?;
    let publication = artifact_path(&case, mission, phase.as_str(), 1);
    private_dir(publication.parent().ok_or("artifact path has no parent")?)?;
    symlink(&target, publication)?;

    let error = match workspace.bind_fixture_artifact(&phase, 1, ARTIFACT_NAME, EXPECTED) {
        Err(error) => error,
        Ok(_) => return Err("symlink publication name was admitted".into()),
    };

    assert_eq!(error.kind(), FixtureArtifactErrorKind::InvalidEntry);
    assert_eq!(std::fs::read(target)?, b"outside\n");
    Ok(())
}

#[test]
fn workspace_replacement_after_binding_is_rejected_before_publication() -> TestResult {
    let case = fixture_case("workspace-replacement")?;
    let mission = "mission-artifact-workspace-swap";
    let phase = PhaseId::new("phase-1")?;
    let workspace = workspace(&case, mission)?;
    let publisher = workspace.bind_fixture_artifact(&phase, 1, ARTIFACT_NAME, EXPECTED)?;
    let workspace_path = case.root.join("workspaces").join(mission);
    let moved = case.root.join("workspaces/moved-workspace");
    std::fs::rename(&workspace_path, &moved)?;
    private_dir(&workspace_path)?;

    let error = match publisher.publish() {
        Err(error) => error,
        Ok(_) => return Err("replacement workspace was accepted".into()),
    };

    assert_eq!(error.kind(), FixtureArtifactErrorKind::IdentityChanged);
    assert!(!moved.join("artifacts/phase-1/attempt-1/output.md").exists());
    Ok(())
}

#[test]
fn artifact_directory_replacement_after_binding_is_rejected() -> TestResult {
    let case = fixture_case("directory-replacement")?;
    let mission = "mission-artifact-directory-swap";
    let phase = PhaseId::new("phase-1")?;
    let workspace = workspace(&case, mission)?;
    let publisher = workspace.bind_fixture_artifact(&phase, 1, ARTIFACT_NAME, EXPECTED)?;
    let attempt_directory = artifact_path(&case, mission, phase.as_str(), 1)
        .parent()
        .ok_or("artifact path has no parent")?
        .to_path_buf();
    let moved = attempt_directory.with_file_name("moved-attempt");
    std::fs::rename(&attempt_directory, &moved)?;
    private_dir(&attempt_directory)?;

    let error = match publisher.publish() {
        Err(error) => error,
        Ok(_) => return Err("replacement artifact directory was accepted".into()),
    };

    assert_eq!(error.kind(), FixtureArtifactErrorKind::IdentityChanged);
    assert!(!moved.join(ARTIFACT_NAME).exists());
    Ok(())
}

#[test]
fn root_replacement_after_binding_is_rejected() -> TestResult {
    let case = fixture_case("root-replacement")?;
    let mission = "mission-artifact-root-swap";
    let phase = PhaseId::new("phase-1")?;
    let workspace = workspace(&case, mission)?;
    let publisher = workspace.bind_fixture_artifact(&phase, 1, ARTIFACT_NAME, EXPECTED)?;
    let moved = case.parent.join("moved-fixture-root");
    std::fs::rename(&case.root, &moved)?;
    private_dir(&case.root)?;

    let error = match publisher.publish() {
        Err(error) => error,
        Ok(_) => return Err("replacement fixture root was accepted".into()),
    };

    assert_eq!(error.kind(), FixtureArtifactErrorKind::IdentityChanged);
    assert!(
        !moved
            .join("workspaces")
            .join(mission)
            .join("artifacts/phase-1/attempt-1/output.md")
            .exists()
    );
    Ok(())
}

#[test]
fn file_replacement_after_publication_invalidates_attestation() -> TestResult {
    let case = fixture_case("file-replacement")?;
    let mission = "mission-artifact-file-swap";
    let phase = PhaseId::new("phase-1")?;
    let workspace = workspace(&case, mission)?;
    let publisher = workspace.bind_fixture_artifact(&phase, 1, ARTIFACT_NAME, EXPECTED)?;
    let attestor = publisher.publish()?;
    let path = artifact_path(&case, mission, phase.as_str(), 1);
    std::fs::rename(&path, path.with_extension("old"))?;
    private_file(&path, EXPECTED)?;

    let error = match attestor.attest() {
        Err(error) => error,
        Ok(_) => return Err("replacement artifact inode was attested".into()),
    };

    assert_eq!(error.kind(), FixtureArtifactErrorKind::IdentityChanged);
    Ok(())
}

#[test]
fn in_place_content_tamper_invalidates_attestation() -> TestResult {
    let case = fixture_case("content-tamper")?;
    let mission = "mission-artifact-content-tamper";
    let phase = PhaseId::new("phase-1")?;
    let workspace = workspace(&case, mission)?;
    let publisher = workspace.bind_fixture_artifact(&phase, 1, ARTIFACT_NAME, EXPECTED)?;
    let attestor = publisher.publish()?;
    std::fs::write(
        artifact_path(&case, mission, phase.as_str(), 1),
        b"tampered bytes\n",
    )?;

    let error = match attestor.attest() {
        Err(error) => error,
        Ok(_) => return Err("changed artifact bytes were attested".into()),
    };

    assert_eq!(error.kind(), FixtureArtifactErrorKind::ContentMismatch);
    Ok(())
}

#[test]
fn debug_and_errors_redact_paths_names_bytes_and_digest() -> TestResult {
    let case = fixture_case("secret-8675309")?;
    let mission = "mission-secret-8675309";
    let phase = PhaseId::new("phase-secret-8675309")?;
    let workspace = workspace(&case, mission)?;
    let publisher = workspace.bind_fixture_artifact(
        &phase,
        1,
        "secret-artifact-8675309.md",
        b"secret-content-8675309",
    )?;
    let publisher_debug = format!("{publisher:?}");
    let attestor = publisher.publish()?;
    let attestor_debug = format!("{attestor:?}");
    let receipt = attestor.attest()?;
    let receipt_debug = format!("{receipt:?}");
    let path = case
        .root
        .join("workspaces")
        .join(mission)
        .join("artifacts")
        .join(phase.as_str())
        .join("attempt-1")
        .join("secret-artifact-8675309.md");
    std::fs::write(&path, b"changed-secret-8675309")?;
    let error = match attestor.attest() {
        Err(error) => error,
        Ok(_) => return Err("tampered secret artifact was attested".into()),
    };
    let outputs = [
        publisher_debug,
        attestor_debug,
        receipt_debug,
        format!("{error:?}"),
        error.to_string(),
    ];

    for output in outputs {
        assert!(!output.contains("8675309"), "{output}");
        assert!(!output.contains("secret-content"), "{output}");
        assert!(!output.contains(receipt.digest()), "{output}");
        assert!(
            !output.contains(case.root.to_string_lossy().as_ref()),
            "{output}"
        );
    }
    Ok(())
}
