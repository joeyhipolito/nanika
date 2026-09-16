use orchestrator_app::{
    DirectoryProbe, FixtureAdmissionPolicy, FreshFixtureAuthority, HomeInputs, IsolatedFixtureRoot,
    RuntimeHomeResolver,
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Default)]
struct FakeProbe {
    directories: BTreeSet<std::path::PathBuf>,
}

impl DirectoryProbe for FakeProbe {
    fn exists(&self, path: &Path) -> bool {
        self.directories.contains(path)
    }
}

fn test_root(name: &str) -> std::path::PathBuf {
    let temp = std::env::temp_dir();
    std::fs::canonicalize(&temp)
        .unwrap_or(temp)
        .join(format!("orchestrator-rs-{name}-{}", std::process::id()))
}

struct AdmittedFixture {
    parent: PathBuf,
    root: PathBuf,
    authority: FreshFixtureAuthority,
}

impl Drop for AdmittedFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

fn admitted_fixture(name: &str) -> Result<AdmittedFixture, Box<dyn std::error::Error>> {
    let parent = test_root(name);
    let _ = std::fs::remove_dir_all(&parent);
    std::fs::create_dir_all(&parent)?;
    let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
    let root = isolated.path().to_path_buf();
    let canonical_temp = std::fs::canonicalize(std::env::temp_dir())?;
    let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
    let policy = FixtureAdmissionPolicy::new(parent.join("live-user"), checkout, canonical_temp);
    let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
    Ok(AdmittedFixture {
        parent,
        root,
        authority,
    })
}

#[test]
fn test_execution_rejects_a_mismatched_home_outside_its_fixture()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = admitted_fixture("deny-mismatch")?;
    let outside_alluka = fixture.parent.join("outside-user/.alluka");
    let mut inputs = HomeInputs::from_user_home(fixture.root.join("claimed-user"));
    inputs.orchestrator_config_dir = Some(outside_alluka.clone());
    let resolved = RuntimeHomeResolver::resolve(&inputs, &FakeProbe::default())?;

    assert!(resolved.authorize_fixture(&fixture.authority).is_err());
    assert!(!outside_alluka.exists());
    Ok(())
}

#[test]
fn test_execution_rejects_parent_directory_alias() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = admitted_fixture("deny-parent-alias")?;
    let outside = fixture.parent.join("outside/.alluka");
    let mut inputs = HomeInputs::from_user_home(fixture.root.join("user"));
    inputs.orchestrator_config_dir = Some(fixture.root.join("nested/../../outside/.alluka"));
    let resolved = RuntimeHomeResolver::resolve(&inputs, &FakeProbe::default())?;

    assert!(resolved.authorize_fixture(&fixture.authority).is_err());
    assert!(!outside.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn test_execution_rejects_symlink_escape() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = admitted_fixture("deny-symlink")?;
    let outside = fixture.parent.join("outside/.alluka");
    std::fs::create_dir_all(&outside)?;
    std::os::unix::fs::symlink(&outside, fixture.root.join("alias"))?;
    let mut inputs = HomeInputs::from_user_home(fixture.root.join("user"));
    inputs.orchestrator_config_dir = Some(fixture.root.join("alias/runtime"));
    let resolved = RuntimeHomeResolver::resolve(&inputs, &FakeProbe::default())?;

    assert!(resolved.authorize_fixture(&fixture.authority).is_err());
    assert!(!outside.join("runtime").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn preparation_rejects_symlink_swapped_in_after_authorization()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = admitted_fixture("deny-symlink-swap")?;
    let outside = fixture.parent.join("outside");
    std::fs::create_dir_all(&outside)?;
    let mut inputs = HomeInputs::from_user_home(fixture.root.join("user"));
    inputs.orchestrator_config_dir = Some(fixture.root.join("missing/runtime"));
    let resolved = RuntimeHomeResolver::resolve(&inputs, &FakeProbe::default())?;
    let authorized = resolved.authorize_fixture(&fixture.authority)?;

    std::os::unix::fs::symlink(&outside, fixture.root.join("missing"))?;
    assert!(authorized.prepare().is_err());
    assert!(!outside.join("runtime").exists());
    Ok(())
}

#[test]
fn admitted_fixture_authority_can_authorize_and_prepare_home()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = admitted_fixture("fixture")?;
    let runtime_home = fixture.root.join("config");
    let mut inputs = HomeInputs::from_user_home(fixture.root.join("user"));
    inputs.orchestrator_config_dir = Some(runtime_home.clone());
    let resolved = RuntimeHomeResolver::resolve(&inputs, &FakeProbe::default())?;
    let authorized = resolved.authorize_fixture(&fixture.authority)?;

    authorized.prepare()?;
    assert!(runtime_home.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&runtime_home)?.permissions().mode() & 0o777,
            0o700
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn fixture_prepare_reverifies_the_admitted_boundary_identity()
-> Result<(), Box<dyn std::error::Error>> {
    use orchestrator_app::ApplicationError;
    use std::os::unix::fs::PermissionsExt;

    let fixture = admitted_fixture("identity-swap")?;
    let runtime_home = fixture.root.join("missing/runtime");
    let mut inputs = HomeInputs::from_user_home(fixture.root.join("user"));
    inputs.orchestrator_config_dir = Some(runtime_home);
    let resolved = RuntimeHomeResolver::resolve(&inputs, &FakeProbe::default())?;
    let authorized = resolved.authorize_fixture(&fixture.authority)?;

    let displaced = fixture.parent.join("displaced-fixture");
    std::fs::rename(&fixture.root, &displaced)?;
    std::fs::create_dir(&fixture.root)?;
    std::fs::set_permissions(&fixture.root, std::fs::Permissions::from_mode(0o700))?;
    assert!(matches!(
        authorized.prepare(),
        Err(ApplicationError::FixtureRuntimeHomeAuthorityUnavailable)
    ));
    assert!(!fixture.root.join("missing/runtime").exists());
    assert!(!displaced.join("missing/runtime").exists());
    Ok(())
}

#[test]
fn resolver_preserves_documented_precedence() -> Result<(), Box<dyn std::error::Error>> {
    let user_home = test_root("precedence");
    let mut inputs = HomeInputs::from_user_home(&user_home);
    inputs.orchestrator_config_dir = Some(Path::new("orchestrator-override").to_path_buf());
    inputs.alluka_home = Some(Path::new("alluka-override").to_path_buf());
    inputs.via_home = Some(Path::new("via-override").to_path_buf());
    let resolved = RuntimeHomeResolver::resolve(&inputs, &FakeProbe::default())?;
    assert_eq!(resolved.path(), Path::new("orchestrator-override"));

    inputs.orchestrator_config_dir = None;
    let resolved = RuntimeHomeResolver::resolve(&inputs, &FakeProbe::default())?;
    assert_eq!(resolved.path(), Path::new("alluka-override"));

    inputs.alluka_home = None;
    let resolved = RuntimeHomeResolver::resolve(&inputs, &FakeProbe::default())?;
    assert_eq!(resolved.path(), Path::new("via-override/orchestrator"));

    inputs.via_home = None;
    let alluka = user_home.join(".alluka");
    let mut probe = FakeProbe::default();
    probe.directories.insert(alluka.clone());
    let resolved = RuntimeHomeResolver::resolve(&inputs, &probe)?;
    assert_eq!(resolved.path(), alluka);

    let resolved = RuntimeHomeResolver::resolve(&inputs, &FakeProbe::default())?;
    assert_eq!(resolved.path(), user_home.join(".via"));
    Ok(())
}
