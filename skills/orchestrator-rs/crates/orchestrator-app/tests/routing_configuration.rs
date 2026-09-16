use orchestrator_app::{
    DirectoryProbe, HomeInputs, ReadCapability, ResolvedRuntimeHome, RoutingConfigError,
    RuntimeHomeResolver, load_routing_map, routing_map_for_run,
};
use std::{
    io,
    path::{Path, PathBuf},
};

const VALID: &[u8] = include_bytes!("../../../tests/fixtures/core-parity/routing-valid.yaml");
const MALFORMED: &[u8] =
    include_bytes!("../../../tests/fixtures/core-parity/routing-malformed.yaml");

#[derive(Default)]
struct NeverExists;

impl DirectoryProbe for NeverExists {
    fn exists(&self, _path: &Path) -> bool {
        false
    }
}

struct MemoryReader(Option<&'static [u8]>);

impl ReadCapability for MemoryReader {
    fn read_relative(
        &self,
        home: &ResolvedRuntimeHome,
        relative: &Path,
    ) -> io::Result<Option<Vec<u8>>> {
        assert_eq!(home.path(), Path::new("/fixture/runtime"));
        assert_eq!(relative, Path::new("config.yaml"));
        Ok(self.0.map(<[u8]>::to_vec))
    }
}

fn fixture_home() -> Result<ResolvedRuntimeHome, Box<dyn std::error::Error>> {
    Ok(RuntimeHomeResolver::resolve(
        &HomeInputs {
            user_home: PathBuf::from("/fixture/user"),
            orchestrator_config_dir: Some(PathBuf::from("/fixture/runtime")),
            alluka_home: None,
            via_home: None,
        },
        &NeverExists,
    )?)
}

#[test]
fn missing_and_valid_routing_fixtures_are_hermetic() -> Result<(), Box<dyn std::error::Error>> {
    let home = fixture_home()?;
    assert!(
        load_routing_map(&home, &MemoryReader(None))?
            .model_tiers
            .is_empty()
    );

    let map = load_routing_map(&home, &MemoryReader(Some(VALID)))?;
    let work = map.model_tiers.get("work").ok_or("missing work tier")?;
    assert_eq!(work.runtime, "codex");
    assert_eq!(work.model, "fixture-work-model");
    Ok(())
}

#[test]
fn malformed_routing_degrades_deterministically_and_warns_only_when_verbose()
-> Result<(), Box<dyn std::error::Error>> {
    let home = fixture_home()?;
    let error = load_routing_map(&home, &MemoryReader(Some(MALFORMED)));
    assert!(matches!(error, Err(RoutingConfigError::Malformed(_))));

    let (_, quiet_warning) = routing_map_for_run(error, false);
    assert_eq!(quiet_warning, None);

    let error = load_routing_map(&home, &MemoryReader(Some(MALFORMED)));
    let (fallback, verbose_warning) = routing_map_for_run(error, true);
    assert!(fallback.model_tiers.is_empty());
    assert!(verbose_warning.is_some());
    Ok(())
}
