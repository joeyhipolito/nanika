#![allow(dead_code)]

use serde::Deserialize;
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path},
};

use super::oracle::sha256_bytes;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const TOP_LEVEL: &[&str] = &[
    "adapter",
    "archive_manifest",
    "case_manifest",
    "cases",
    "documented_normalization",
    "module_proxy_manifest",
    "oracle_revision",
    "schema_version",
];
const CASE_FIELDS: &[&str] = &[
    "case_id",
    "comparison",
    "contracts",
    "environment",
    "expected",
    "expected_go_stderr",
    "expected_rust_error",
    "fixed_ids",
    "fixed_time",
    "go_api",
    "home_layout",
    "input",
    "normalization",
    "operation",
    "oracle",
];
const REQUIRED_CASE_FIELDS: &[&str] = &[
    "case_id",
    "comparison",
    "contracts",
    "environment",
    "fixed_ids",
    "fixed_time",
    "go_api",
    "input",
    "normalization",
    "operation",
    "oracle",
];
const ENVIRONMENT_FIELDS: &[&str] = &[
    "ALLUKA_HOME",
    "HOME",
    "NANIKA_CODEX_AUTO",
    "NANIKA_DEFAULT_RUNTIME",
    "ORCHESTRATOR_CONFIG_DIR",
    "ORCHESTRATOR_PERSONAS_DIR",
    "ORCHESTRATOR_STALL_TIMEOUT",
    "PATH",
    "TMPDIR",
    "VIA_HOME",
];
const NORMALIZATION_FIELDS: &[&str] = &[
    "built_in_model_input",
    "differential_label_removed",
    "error_class",
    "file_order",
    "full_decode",
    "phase_map_sorted_by_key",
    "plan_fields_omitted",
    "replay_strictness",
    "rust_policy_fixture",
    "selection_source",
    "source_shape",
    "unknown_preservation",
    "writer",
];
const POLICY_FIELDS: &[&str] = &["persona", "role", "runtime", "selection_method", "tier"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseManifest {
    schema_version: u64,
    canonicalization: String,
    cases: Vec<CaseIdentity>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseIdentity {
    case_id: String,
    case_sha256: String,
    references: Vec<ReferenceIdentity>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceIdentity {
    path: String,
    sha256: String,
}

pub fn validate(corpus: &Value, fixture_root: &Path, manifest_path: &Path) -> TestResult {
    let root = corpus
        .as_object()
        .ok_or("differential corpus must be an object")?;
    exact_fields(root, TOP_LEVEL, TOP_LEVEL, "corpus")?;
    if corpus["schema_version"] != 2 {
        return Err("unsupported differential corpus schema".into());
    }
    let declared_manifest = corpus["case_manifest"]
        .as_str()
        .ok_or("case_manifest must be a string")?;
    if !manifest_path.ends_with(declared_manifest) {
        return Err("case manifest path does not match the corpus declaration".into());
    }
    let cases = corpus["cases"]
        .as_array()
        .ok_or("differential corpus cases must be an array")?;
    let manifest: CaseManifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
    if manifest.schema_version != 1
        || manifest.canonicalization
            != "UTF-8 JSON with recursively sorted object keys and no insignificant whitespace"
    {
        return Err("unsupported case manifest schema or canonicalization".into());
    }
    if manifest.cases.len() != cases.len() {
        return Err("case manifest length does not match corpus".into());
    }

    let mut previous: Option<&str> = None;
    for (index, (case, identity)) in cases.iter().zip(&manifest.cases).enumerate() {
        let object = case
            .as_object()
            .ok_or_else(|| format!("case {index} must be an object"))?;
        exact_fields(object, CASE_FIELDS, REQUIRED_CASE_FIELDS, "case")?;
        let case_id = case["case_id"].as_str().ok_or("case_id must be a string")?;
        if !safe_relative(Path::new(case_id)) {
            return Err(format!("unsafe differential case id {case_id:?}").into());
        }
        if previous.is_some_and(|value| value >= case_id) {
            return Err("differential cases must have unique lexicographically ordered IDs".into());
        }
        previous = Some(case_id);
        validate_case_fields(case_id, object)?;
        if identity.case_id != case_id {
            return Err(format!("case manifest identity mismatch at {case_id}").into());
        }
        if sha256_bytes(&serde_json::to_vec(case)?)? != identity.case_sha256 {
            return Err(format!("case manifest hash mismatch for {case_id}").into());
        }
        validate_references(case_id, case, identity, fixture_root)?;
    }
    Ok(())
}

fn validate_case_fields(case_id: &str, case: &Map<String, Value>) -> TestResult {
    let environment = case["environment"]
        .as_object()
        .ok_or("case environment must be an object")?;
    exact_fields(
        environment,
        ENVIRONMENT_FIELDS,
        ENVIRONMENT_FIELDS,
        "environment",
    )?;
    for (name, value) in environment {
        if !value.is_null() && !value.is_string() {
            return Err(format!("{case_id}: environment {name} must be string or null").into());
        }
    }
    exact_fields(
        case["normalization"]
            .as_object()
            .ok_or("normalization must be an object")?,
        NORMALIZATION_FIELDS,
        &[],
        "normalization",
    )?;
    let operation = case["operation"]
        .as_str()
        .ok_or("operation must be a string")?;
    let allowed = match operation {
        "checkpoint" => &["bytes", "bytes_file"][..],
        "checkpoint_writer" => &["domain", "phase", "started_at", "status", "workspace_id"],
        "event_writer" => &[
            "data",
            "id",
            "mission_id",
            "phase_id",
            "sequence",
            "timestamp",
            "type",
            "worker_id",
        ],
        "events" | "routing" => &["bytes_file"],
        "home" => &[],
        "mission" => &[
            "default_policy",
            "known_personas",
            "persona_dir",
            "phase_policies",
            "source",
            "source_file",
            "target_context",
            "target_phase_policies",
        ],
        "model" => &[
            "built_in_model",
            "effective_runtime",
            "forced_model",
            "routing",
            "tier",
        ],
        "projection" => &["definitions", "events", "mission_id"],
        "runtime" => &[
            "configured_runtime",
            "forced_runtime",
            "policy_runtime",
            "tier",
        ],
        "stall" => &["flag_value", "phase_timeout", "worker_default"],
        other => return Err(format!("{case_id}: unsupported operation {other:?}").into()),
    };
    exact_fields(
        case["input"]
            .as_object()
            .ok_or("case input must be an object")?,
        allowed,
        &[],
        "operation input",
    )?;
    if operation == "mission" {
        validate_mission(case_id, &case["input"])?;
    }
    if let Some(expected) = case.get("expected") {
        exact_fields(
            expected.as_object().ok_or("expected must be an object")?,
            &["observation_file", "ok"],
            &[],
            "expected",
        )?;
    }
    Ok(())
}

fn validate_mission(case_id: &str, input: &Value) -> TestResult {
    let target = input
        .get("target_context")
        .and_then(Value::as_object)
        .ok_or("mission target_context must be an object")?;
    exact_fields(
        target,
        &["preferred_personas", "skip_review_injection"],
        &["preferred_personas", "skip_review_injection"],
        "target_context",
    )?;
    exact_fields(
        input["default_policy"]
            .as_object()
            .ok_or("mission default policy must be an object")?,
        POLICY_FIELDS,
        POLICY_FIELDS,
        "mission policy",
    )?;
    for group in ["phase_policies", "target_phase_policies"] {
        for policy in input[group]
            .as_object()
            .ok_or("mission phase policies must be an object")?
            .values()
        {
            exact_fields(
                policy
                    .as_object()
                    .ok_or("mission phase policy must be an object")?,
                POLICY_FIELDS,
                POLICY_FIELDS,
                "mission phase policy",
            )?;
        }
    }
    if input.get("source").is_some() == input.get("source_file").is_some() {
        return Err(format!("{case_id}: mission must have exactly one source form").into());
    }
    Ok(())
}

fn validate_references(
    case_id: &str,
    case: &Value,
    identity: &CaseIdentity,
    fixture_root: &Path,
) -> TestResult {
    let mut declared = BTreeMap::new();
    for reference in &identity.references {
        if declared
            .insert(reference.path.as_str(), reference.sha256.as_str())
            .is_some()
        {
            return Err(format!("duplicate case reference for {case_id}").into());
        }
    }
    let mut expected = BTreeSet::new();
    collect_references(case, &mut expected);
    if declared.keys().copied().collect::<BTreeSet<_>>() != expected {
        return Err(format!("case reference set mismatch for {case_id}").into());
    }
    for (path, digest) in declared {
        let relative = Path::new(path);
        if !safe_relative(relative) {
            return Err(format!("unsafe case reference {path:?}").into());
        }
        let source = fixture_root.join(relative);
        reject_symlink_components(fixture_root, &source)?;
        if !fs::symlink_metadata(&source)?.file_type().is_file() {
            return Err(format!("case reference is not a regular file: {path}").into());
        }
        if sha256_bytes(&fs::read(source)?)? != digest {
            return Err(format!("case reference hash mismatch for {case_id}: {path}").into());
        }
    }
    Ok(())
}

fn reject_symlink_components(root: &Path, path: &Path) -> TestResult {
    let relative = path.strip_prefix(root)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(
                    format!("case reference traverses symlink: {}", current.display()).into(),
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn collect_references<'a>(value: &'a Value, paths: &mut BTreeSet<&'a str>) {
    match value {
        Value::Object(values) => {
            for (name, value) in values {
                if name.ends_with("_file") {
                    if let Some(path) = value.as_str() {
                        paths.insert(path);
                    }
                }
                collect_references(value, paths);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_references(value, paths);
            }
        }
        _ => {}
    }
}

fn exact_fields(
    object: &Map<String, Value>,
    allowed: &[&str],
    required: &[&str],
    context: &str,
) -> TestResult {
    let allowed = allowed.iter().copied().collect::<BTreeSet<_>>();
    for name in object.keys() {
        if !allowed.contains(name.as_str()) {
            return Err(format!("unknown {context} field {name:?}").into());
        }
    }
    for name in required {
        if !object.contains_key(*name) {
            return Err(format!("missing {context} field {name:?}").into());
        }
    }
    Ok(())
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}
