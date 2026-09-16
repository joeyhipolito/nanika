use orchestrator_app::{
    DirectoryProbe, HomeInputs, ReadCapability, ResolvedRuntimeHome, RoutingConfigError,
    RuntimeHomeResolver, load_bundled_conformance_ledger, load_routing_map,
};
use orchestrator_core::RoutingMap;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const VALID: &[u8] = include_bytes!("../../../tests/fixtures/core-parity/routing-valid.yaml");
const MALFORMED: &[u8] =
    include_bytes!("../../../tests/fixtures/core-parity/routing-malformed.yaml");
const GOLDEN: &str = include_str!("../../../tests/fixtures/core-parity/yaml-msrv-golden.json");

#[derive(Default)]
struct NeverExists;

impl DirectoryProbe for NeverExists {
    fn exists(&self, _path: &Path) -> bool {
        false
    }
}

struct MemoryReader(Option<Vec<u8>>);

impl ReadCapability for MemoryReader {
    fn read_relative(
        &self,
        home: &ResolvedRuntimeHome,
        relative: &Path,
    ) -> io::Result<Option<Vec<u8>>> {
        assert_eq!(home.path(), Path::new("/fixture/runtime"));
        assert_eq!(relative, Path::new("config.yaml"));
        Ok(self.0.clone())
    }
}

fn fixture_home() -> TestResult<ResolvedRuntimeHome> {
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
fn bundled_ledger_retains_all_forty_two_contracts() -> TestResult {
    let ledger = load_bundled_conformance_ledger()?;
    assert_eq!(ledger.contracts.len(), 42);
    Ok(())
}

#[test]
fn routing_missing_valid_and_malformed_behavior_is_unchanged() -> TestResult {
    let home = fixture_home()?;
    assert!(
        load_routing_map(&home, &MemoryReader(None))?
            .model_tiers
            .is_empty()
    );
    assert!(load_routing_map(&home, &MemoryReader(Some(VALID.to_vec()))).is_ok());
    assert!(matches!(
        load_routing_map(&home, &MemoryReader(Some(MALFORMED.to_vec()))),
        Err(RoutingConfigError::Malformed(_))
    ));
    Ok(())
}

#[test]
fn patched_parser_matches_upstream_029_classification_golden() -> TestResult {
    let golden: Value = serde_json::from_str(GOLDEN)?;
    assert_eq!(golden["upstream"], "serde-saphyr 0.0.29");

    let deep = deeply_nested_yaml(70);
    let aliases = excessive_alias_yaml(101);
    let cases = BTreeMap::from([
        (
            "alias-expansion",
            (aliases.as_str(), classification(&aliases)),
        ),
        (
            "anchors-aliases",
            (
                "model_tiers:\n  work: &tier\n    runtime: codex\n    model: fixture\n  quick: *tier\n",
                classification(
                    "model_tiers:\n  work: &tier\n    runtime: codex\n    model: fixture\n  quick: *tier\n",
                ),
            ),
        ),
        (
            "duplicate-keys",
            (
                "model_tiers:\n  work:\n    runtime: codex\n    runtime: claude\n",
                classification("model_tiers:\n  work:\n    runtime: codex\n    runtime: claude\n"),
            ),
        ),
        ("excessive-depth", (deep.as_str(), classification(&deep))),
        (
            "malformed-syntax",
            (
                std::str::from_utf8(MALFORMED)?,
                classification(std::str::from_utf8(MALFORMED)?),
            ),
        ),
        (
            "multiple-documents",
            (
                "model_tiers: {}\n---\nmodel_tiers: {}\n",
                classification("model_tiers: {}\n---\nmodel_tiers: {}\n"),
            ),
        ),
        (
            "recursive-alias",
            (
                "model_tiers: &self\n  work: *self\n",
                classification("model_tiers: &self\n  work: *self\n"),
            ),
        ),
        (
            "tags",
            (
                "!!map\nmodel_tiers:\n  work:\n    runtime: codex\n",
                classification("!!map\nmodel_tiers:\n  work:\n    runtime: codex\n"),
            ),
        ),
        (
            "type-mismatch",
            (
                "model_tiers: [work]\n",
                classification("model_tiers: [work]\n"),
            ),
        ),
        (
            "unknown-fields",
            (
                "unknown_root: retained-by-yaml\nmodel_tiers: {}\n",
                classification("unknown_root: retained-by-yaml\nmodel_tiers: {}\n"),
            ),
        ),
        (
            "valid-routing",
            (
                std::str::from_utf8(VALID)?,
                classification(std::str::from_utf8(VALID)?),
            ),
        ),
    ]);

    for (name, (source, actual)) in cases {
        let expected = golden["classifications"][name]
            .as_str()
            .ok_or_else(|| format!("missing golden classification for {name}"))?;
        assert_eq!(actual, expected, "classification drift for {name}");
        let started = Instant::now();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            serde_saphyr::from_str::<RoutingMap>(source)
        }));
        assert!(outcome.is_ok(), "parser panicked for {name}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "parser exceeded resource bound for {name}: {:?}",
            started.elapsed()
        );
    }
    Ok(())
}

fn classification(source: &str) -> &'static str {
    if serde_saphyr::from_str::<RoutingMap>(source).is_ok() {
        "accepted"
    } else {
        "rejected"
    }
}

fn deeply_nested_yaml(depth: usize) -> String {
    let mut source = String::from("model_tiers:\n  work:\n    runtime: codex\nunknown:");
    for index in 0..depth {
        source.push_str(&format!("\n{}level{index}:", "  ".repeat(index + 1)));
    }
    source.push_str(&format!("\n{}value: end\n", "  ".repeat(depth + 1)));
    source
}

fn excessive_alias_yaml(count: usize) -> String {
    let aliases = std::iter::repeat_n("*tier", count)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "model_tiers:\n  work: &tier {{ runtime: codex, model: fixture }}\nunknown: [{aliases}]\n"
    )
}
