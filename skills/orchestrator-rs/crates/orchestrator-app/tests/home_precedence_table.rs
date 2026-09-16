use orchestrator_app::{DirectoryProbe, HomeInputs, HomeSelection, RuntimeHomeResolver};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Default)]
struct FixtureProbe {
    entries: BTreeSet<PathBuf>,
}

impl DirectoryProbe for FixtureProbe {
    fn exists(&self, path: &Path) -> bool {
        self.entries.contains(path)
    }
}

#[test]
fn base_home_precedence_is_table_driven_and_never_reads_a_live_home()
-> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        (
            HomeInputs {
                user_home: "/fixture/user".into(),
                orchestrator_config_dir: Some("relative/nonexistent".into()),
                alluka_home: Some("/fixture/alluka".into()),
                via_home: Some("/fixture/via".into()),
            },
            FixtureProbe::default(),
            PathBuf::from("relative/nonexistent"),
            HomeSelection::OrchestratorConfigDir,
        ),
        (
            HomeInputs {
                user_home: "/fixture/user".into(),
                orchestrator_config_dir: None,
                alluka_home: Some("/fixture/alluka".into()),
                via_home: Some("/fixture/via".into()),
            },
            FixtureProbe::default(),
            PathBuf::from("/fixture/alluka"),
            HomeSelection::AllukaHome,
        ),
        (
            HomeInputs {
                user_home: "/fixture/user".into(),
                orchestrator_config_dir: None,
                alluka_home: None,
                via_home: Some("/fixture/via".into()),
            },
            FixtureProbe::default(),
            PathBuf::from("/fixture/via/orchestrator"),
            HomeSelection::ViaHome,
        ),
        (
            HomeInputs::from_user_home("/fixture/user"),
            FixtureProbe {
                entries: BTreeSet::from([PathBuf::from("/fixture/user/.alluka")]),
            },
            PathBuf::from("/fixture/user/.alluka"),
            HomeSelection::ExistingAlluka,
        ),
        (
            HomeInputs::from_user_home("/fixture/user"),
            FixtureProbe::default(),
            PathBuf::from("/fixture/user/.via"),
            HomeSelection::ViaFallback,
        ),
    ];

    for (inputs, probe, expected_path, expected_selection) in cases {
        let resolved = RuntimeHomeResolver::resolve(&inputs, &probe)?;
        assert_eq!(resolved.path(), expected_path);
        assert_eq!(resolved.selection(), expected_selection);
    }
    Ok(())
}
