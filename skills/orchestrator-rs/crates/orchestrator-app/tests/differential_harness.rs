mod support;

use orchestrator_app::{
    DirectoryProbe, HomeInputs, RoutingConfigError, RuntimeHomeResolver, project_event,
    routing_map_for_run,
};
use orchestrator_core::{
    AuthoredParseContext, CheckpointPhase, CheckpointPlan, CheckpointProjection, EventJsonMap,
    EventRecord, ExecutionMode, MissionId, MissionState, ModelResolutionInput, PersonaCatalog,
    PhaseDefinition, PhaseId, PhasePolicyFixture, RoutingMap, RoutingTier, RuntimePolicyFixture,
    RuntimeResolutionInput, StallResolutionInput, StallSource, StallTimeoutValue,
    TargetContextFixture, decode_checkpoint, encode_current_checkpoint, encode_current_event,
    parse_authored_phases, reduce, resolve_model, resolve_runtime, resolve_stall_timeout,
    scan_event_log,
};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};
use support::corpus;
use support::oracle::{
    OracleFixture, sandboxed_command, validate_case_environment, validate_case_path,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const REQUIRED_ENVIRONMENT: [&str; 10] = [
    "HOME",
    "ORCHESTRATOR_CONFIG_DIR",
    "ALLUKA_HOME",
    "VIA_HOME",
    "ORCHESTRATOR_PERSONAS_DIR",
    "NANIKA_DEFAULT_RUNTIME",
    "NANIKA_CODEX_AUTO",
    "ORCHESTRATOR_STALL_TIMEOUT",
    "TMPDIR",
    "PATH",
];
const CORPUS: &str = include_str!("../../../tests/fixtures/core-parity/differential/corpus.json");

#[test]
fn hermetic_go_rust_differential_corpus() -> TestResult {
    let corpus: Value = serde_json::from_str(CORPUS)?;
    let fixture_sources =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/core-parity");
    let case_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/core-parity/differential/case-manifest.json");
    corpus::validate(&corpus, &fixture_sources, &case_manifest)?;
    let cases = corpus["cases"]
        .as_array()
        .ok_or("differential corpus cases must be an array")?;
    for unsafe_id in ["", "../outside", "nested/../../outside", "/tmp/outside"] {
        assert!(
            fixture_case_path(Path::new("/fixture"), unsafe_id).is_err(),
            "accepted fixture-escaping case id {unsafe_id:?}"
        );
    }
    let oracle = OracleFixture::build()?;
    let fixture = oracle.root().to_path_buf();
    let outside = fixture.join("outside");
    fs::create_dir_all(&outside)?;
    fs::write(outside.join("sentinel"), b"unchanged")?;
    let outside_before = tree_manifest(&outside)?;
    for case in cases {
        run_case(case, &fixture, oracle.binary())?;
    }

    if tree_manifest(&outside)? != outside_before {
        return Err("a differential subprocess wrote outside its fixture root".into());
    }
    assert_no_socket_or_database_artifacts(&fixture)?;
    assert_no_surviving_fixture_processes(&fixture)?;
    oracle.cleanup()
}

fn run_case(case: &Value, fixture: &Path, oracle: &Path) -> TestResult {
    let case_id = string(case, "case_id")?;
    let operation = string(case, "operation")?;
    let oracle_kind = string(case, "oracle")?;
    if case["fixed_time"].as_str().is_none() || !case["fixed_ids"].is_array() {
        return Err(format!("{case_id}: fixed time and IDs must be explicit").into());
    }
    let case_root = fixture_case_path(fixture, case_id)?;
    fs::create_dir_all(case_root.join("tmp"))?;
    fs::create_dir_all(case_root.join("inert-bin"))?;
    apply_home_layout(case, &case_root)?;

    let environment = case_environment(case, &case_root)?;
    let input = materialize_input(case, &case_root)?;
    assert_case_properties(case_id, operation, &input)?;
    let (rust_operation, rust_input, go_operation, go_input) =
        prepare_observations(operation, &input, &case_root)?;
    let mut rust = rust_observation(rust_operation, &rust_input, &environment)?;
    normalize_fixture_root(&mut rust, &case_root);

    if oracle_kind == "normative-rust" {
        let expected = string(case, "expected_rust_error")?;
        if rust["ok"] != false || rust["error"] != expected {
            return Err(diff_error(case_id, &json!({"ok":false,"error":expected}), &rust).into());
        }
        return Ok(());
    }
    if oracle_kind != "go-package" {
        return Err(format!("{case_id}: unsupported oracle kind {oracle_kind:?}").into());
    }

    let request = serde_json::to_vec(&json!({"operation": go_operation, "input": go_input}))?;
    let output = sandboxed_command(
        oracle,
        &[],
        &environment,
        fixture,
        Some(&request),
        Duration::from_secs(30),
    )?;
    require_success(&format!("running Go oracle case {case_id}"), &output)?;
    let expected_stderr = case.get("expected_go_stderr").and_then(Value::as_str);
    if let Some(expected) = expected_stderr {
        if !String::from_utf8_lossy(&output.stderr).contains(expected) {
            return Err(format!(
                "{case_id}: Go oracle stderr did not contain {expected:?}:\n{}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
    } else if !output.stderr.is_empty() {
        return Err(format!(
            "{case_id}: Go oracle wrote unexpected stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let mut go: Value = serde_json::from_slice(&output.stdout)?;
    normalize_fixture_root(&mut go, &case_root);
    if let Some(expected_file) = case
        .get("expected")
        .and_then(|expected| expected.get("observation_file"))
        .and_then(Value::as_str)
    {
        let relative = Path::new(expected_file);
        if !safe_relative(relative) {
            return Err(format!("{case_id}: unsafe expected observation path").into());
        }
        let fixture_root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/core-parity");
        let expected: Value = serde_json::from_slice(&fs::read(fixture_root.join(relative))?)?;
        if go != expected {
            return Err(diff_error(case_id, &expected, &go).into());
        }
    }
    if go != rust {
        return Err(diff_error(case_id, &go, &rust).into());
    }
    Ok(())
}

fn prepare_observations<'a>(
    operation: &'a str,
    input: &Value,
    case_root: &Path,
) -> TestResult<(&'a str, Value, &'a str, Value)> {
    match operation {
        "mission" => {
            let go = json!({
                "source": string(input, "source")?,
                "persona_dir": string(input, "persona_dir")?,
                "target_context": input.get("target_context").cloned().ok_or("mission target_context missing")?
            });
            Ok((operation, input.clone(), operation, go))
        }
        "checkpoint_writer" => {
            let bytes = encode_checkpoint_fixture(input)?;
            let materialized = json!({"bytes":String::from_utf8(bytes)?});
            prepare_observations("checkpoint", &materialized, case_root)
        }
        "event_writer" => {
            let bytes = encode_event_fixture(input)?;
            let materialized = json!({"bytes":String::from_utf8(bytes)?});
            prepare_observations("events", &materialized, case_root)
        }
        "routing" => {
            let config_dir = case_root.join("go-routing");
            fs::create_dir_all(&config_dir)?;
            let present = input
                .get("present")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if present {
                fs::write(config_dir.join("config.yaml"), string(input, "bytes")?)?;
            }
            Ok((
                operation,
                input.clone(),
                operation,
                json!({"config_dir":config_dir}),
            ))
        }
        "checkpoint" => {
            let workspace = case_root.join("go-checkpoint");
            fs::create_dir_all(&workspace)?;
            let bytes = string(input, "bytes")?;
            fs::write(workspace.join("checkpoint.json"), bytes)?;
            Ok((
                operation,
                input.clone(),
                operation,
                json!({"path":workspace}),
            ))
        }
        "events" => {
            let path = case_root.join("go-events.jsonl");
            fs::write(&path, string(input, "bytes")?)?;
            Ok((operation, input.clone(), operation, json!({"path":path})))
        }
        "projection" => {
            let config_dir = case_root.join("go-projection");
            let events_dir = config_dir.join("events");
            fs::create_dir_all(&events_dir)?;
            let mission_id = string(input, "mission_id")?;
            let mut bytes = Vec::new();
            for event in input["events"]
                .as_array()
                .ok_or("projection events must be an array")?
            {
                serde_json::to_writer(&mut bytes, event)?;
                bytes.push(b'\n');
            }
            fs::write(events_dir.join(format!("{mission_id}.jsonl")), bytes)?;
            Ok((
                operation,
                input.clone(),
                operation,
                json!({"mission_id":mission_id,"config_dir":config_dir}),
            ))
        }
        "runtime" => Ok((
            operation,
            input.clone(),
            operation,
            json!({
                "forced_runtime": string(input, "forced_runtime")?,
                "configured_runtime": string(input, "configured_runtime")?,
                "policy_runtime": string(input, "policy_runtime")?,
                "tier": string(input, "tier")?
            }),
        )),
        "model" => Ok((
            operation,
            input.clone(),
            operation,
            json!({
                "tier": string(input, "tier")?,
                "effective_runtime": string(input, "effective_runtime")?,
                "routing": input.get("routing").cloned().ok_or("model routing missing")?
            }),
        )),
        "home" => Ok((operation, input.clone(), operation, json!({}))),
        _ => Ok((operation, input.clone(), operation, input.clone())),
    }
}

fn encode_checkpoint_fixture(input: &Value) -> TestResult<Vec<u8>> {
    let phase = CheckpointPhase {
        id: string(&input["phase"], "id")?.to_owned(),
        name: string(&input["phase"], "name")?.to_owned(),
        objective: string(&input["phase"], "objective")?.to_owned(),
        persona: string(&input["phase"], "persona")?.to_owned(),
        model_tier: string(&input["phase"], "model_tier")?.to_owned(),
        status: string(&input["phase"], "status")?.to_owned(),
        ..CheckpointPhase::default()
    };
    encode_current_checkpoint(&CheckpointProjection {
        workspace_id: string(input, "workspace_id")?.to_owned(),
        domain: string(input, "domain")?.to_owned(),
        plan: Some(CheckpointPlan {
            id: "plan-fixed".to_owned(),
            task: "writer interoperability".to_owned(),
            phases: vec![phase],
            execution_mode: "sequential".to_owned(),
            decomp_source: "predecomposed".to_owned(),
            created_at: string(input, "started_at")?.to_owned(),
            ..CheckpointPlan::default()
        }),
        status: string(input, "status")?.to_owned(),
        started_at: string(input, "started_at")?.to_owned(),
        ..CheckpointProjection::default()
    })
    .map_err(Into::into)
}

fn encode_event_fixture(input: &Value) -> TestResult<Vec<u8>> {
    let data = input["data"].as_object().map(|values| {
        values
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<EventJsonMap>()
    });
    encode_current_event(&EventRecord {
        id: string(input, "id")?.to_owned(),
        event_type: string(input, "type")?.to_owned(),
        timestamp: string(input, "timestamp")?.to_owned(),
        sequence: input["sequence"]
            .as_i64()
            .ok_or("sequence must be an integer")?,
        mission_id: string(input, "mission_id")?.to_owned(),
        phase_id: optional_non_empty(input, "phase_id"),
        worker_id: optional_non_empty(input, "worker_id"),
        data,
        extra: EventJsonMap::default(),
    })
    .map_err(Into::into)
}

fn assert_case_properties(case_id: &str, operation: &str, input: &Value) -> TestResult {
    if operation == "checkpoint" && case_id.contains("unknown-fields") {
        let decoded = decode_checkpoint(string(input, "bytes")?.as_bytes())?;
        if !decoded
            .projection
            .envelope_extra
            .contains_key("envelope_future")
            || !decoded.projection.extra.contains_key("payload_future")
            || decoded
                .projection
                .plan
                .as_ref()
                .and_then(|plan| plan.extra.get("plan_future"))
                .is_none()
            || decoded
                .projection
                .plan
                .as_ref()
                .and_then(|plan| plan.phases.first())
                .and_then(|phase| phase.extra.get("phase_future"))
                .is_none()
        {
            return Err("checkpoint unknown-field preservation assertion failed".into());
        }
    }
    if operation == "events" && case_id.contains("unknown-and-file-order") {
        let scan = scan_event_log(string(input, "bytes")?.as_bytes());
        let unknown = scan.events.get(1).ok_or("missing unknown event")?;
        if !unknown.record.extra.contains_key("future_top")
            || unknown
                .record
                .data
                .as_ref()
                .and_then(|data| data.get("future_data"))
                .is_none()
            || orchestrator_core::encode_preserved_event(unknown) != unknown.raw_line
        {
            return Err("event unknown/raw preservation assertion failed".into());
        }
    }
    Ok(())
}

fn case_environment(case: &Value, root: &Path) -> TestResult<Vec<(&'static str, String)>> {
    let values = case["environment"]
        .as_object()
        .ok_or("case environment must be an object")?;
    let validated = validate_case_environment(values, &REQUIRED_ENVIRONMENT, root)?
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let mut environment = Vec::new();
    for name in REQUIRED_ENVIRONMENT {
        if let Some(value) = validated.get(name) {
            environment.push((name, value.clone()));
        }
    }
    environment.extend([
        ("ORACLE_FIXED_TIME", string(case, "fixed_time")?.to_owned()),
        (
            "ORACLE_FIXED_IDS",
            serde_json::to_string(&case["fixed_ids"])?,
        ),
        ("TZ", "UTC".to_owned()),
        ("LANG", "C".to_owned()),
        ("LC_ALL", "C".to_owned()),
    ]);
    Ok(environment)
}

fn apply_home_layout(case: &Value, root: &Path) -> TestResult {
    let Some(layout) = case.get("home_layout") else {
        return Ok(());
    };
    for value in layout.as_array().ok_or("home_layout must be an array")? {
        let value = value.as_str().ok_or("home_layout entry must be a string")?;
        let directory = value.ends_with('/');
        let relative = Path::new(value.trim_end_matches('/'));
        if !safe_relative(relative) {
            return Err(format!("unsafe home_layout entry {value:?}").into());
        }
        let path = root.join(relative);
        if directory {
            fs::create_dir_all(path)?;
        } else {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, [])?;
        }
    }
    Ok(())
}

fn materialize_input(case: &Value, root: &Path) -> TestResult<Value> {
    let mut input = case["input"]
        .as_object()
        .ok_or("case input must be an object")?
        .clone();
    for (reference, target) in [("source_file", "source"), ("bytes_file", "bytes")] {
        if let Some(value) = input.remove(reference) {
            let relative = Path::new(value.as_str().ok_or("fixture reference must be a string")?);
            if !safe_relative(relative) {
                return Err(format!("unsafe fixture reference {relative:?}").into());
            }
            let fixture_root =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/core-parity");
            let bytes = fs::read(fixture_root.join(relative))?;
            input.insert(target.to_owned(), Value::String(String::from_utf8(bytes)?));
        }
    }
    let mut input = Value::Object(input);
    replace_tokens(&mut input, root);
    if string(case, "operation")? == "mission" {
        validate_case_path(Path::new(string(&input, "persona_dir")?), root)?;
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/core-parity/personas");
        let destination = root.join("personas");
        fs::create_dir_all(&destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                fs::copy(entry.path(), destination.join(entry.file_name()))?;
            }
        }
    }
    Ok(input)
}

fn replace_tokens(value: &mut Value, root: &Path) {
    match value {
        Value::String(text) => *text = expand_fixture(text, root),
        Value::Array(values) => {
            for value in values {
                replace_tokens(value, root);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                replace_tokens(value, root);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn normalize_fixture_root(value: &mut Value, root: &Path) {
    match value {
        Value::String(text) => {
            *text = text.replace(root.to_string_lossy().as_ref(), "$FIXTURE_ROOT");
        }
        Value::Array(values) => {
            for value in values {
                normalize_fixture_root(value, root);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                normalize_fixture_root(value, root);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn rust_observation(
    operation: &str,
    input: &Value,
    environment: &[(&str, String)],
) -> TestResult<Value> {
    match operation {
        "mission" => observe_mission(input, environment),
        "home" => observe_home(environment),
        "runtime" => observe_runtime(input, environment),
        "model" => observe_model(input),
        "routing" => observe_routing(input),
        "stall" => observe_stall(input, environment),
        "checkpoint" => observe_checkpoint(input),
        "events" => observe_events(input),
        "projection" => observe_projection(input),
        _ => Err(format!("unsupported Rust observation {operation:?}").into()),
    }
}

fn observe_mission(input: &Value, environment: &[(&str, String)]) -> TestResult<Value> {
    let default = policy(&input["default_policy"])?;
    let mut phases = BTreeMap::new();
    for (name, value) in object(input, "phase_policies")? {
        phases.insert(name.clone(), policy(value)?);
    }
    let mut target_phases = BTreeMap::new();
    for (name, value) in object(input, "target_phase_policies")? {
        target_phases.insert(name.clone(), policy(value)?);
    }
    let context = AuthoredParseContext {
        user_home: environment_value(environment, "HOME").map(PathBuf::from),
        persona_catalog: PersonaCatalog {
            names: string_array(input, "known_personas")?.into_iter().collect(),
        },
        target_context: Some(TargetContextFixture {
            phases: target_phases,
        }),
        policy: RuntimePolicyFixture { default, phases },
    };
    let source = string(input, "source")?;
    let parsed = match parse_authored_phases(source, &context) {
        Ok(value) => value,
        Err(_) => return Ok(json!({"ok":false,"error":"no_phases"})),
    };
    let phases = parsed
        .phases
        .iter()
        .map(|phase| {
            json!({
                "id": phase.id.as_str(), "name": phase.name, "objective": phase.objective,
                "persona": phase.persona, "persona_selection_method": phase.persona_selection_method,
                "model_tier": phase.model_tier, "role": phase.role, "skills": phase.skills,
                "constraints": [],
                "dependencies": phase.dependencies.iter().map(PhaseId::as_str).collect::<Vec<_>>(),
                "expected": phase.expected, "target_dir": phase.workdir, "runtime": phase.runtime,
                "runtime_policy_applied": phase.runtime_policy_applied,
                "stall_timeout_ns": phase.stall_timeout.map_or(0_u64, |value| u64::try_from(value.as_nanos()).unwrap_or(u64::MAX)),
                "priority": phase.priority, "status": phase.status
            })
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"ok":true,"observation":{"execution_mode": execution_mode(parsed.execution_mode), "phases":phases}}),
    )
}

fn policy(value: &Value) -> TestResult<PhasePolicyFixture> {
    Ok(PhasePolicyFixture {
        fallback_persona: string(value, "persona")?.to_owned(),
        fallback_selection_method: string(value, "selection_method")?.to_owned(),
        tier: string(value, "tier")?.to_owned(),
        role: string(value, "role")?.to_owned(),
        runtime: string(value, "runtime")?.to_owned(),
    })
}

fn observe_home(environment: &[(&str, String)]) -> TestResult<Value> {
    let home = environment_value(environment, "HOME").ok_or("HOME must be set")?;
    let mut inputs = HomeInputs::from_user_home(home);
    inputs.orchestrator_config_dir =
        environment_value(environment, "ORCHESTRATOR_CONFIG_DIR").map(PathBuf::from);
    inputs.alluka_home = environment_value(environment, "ALLUKA_HOME").map(PathBuf::from);
    inputs.via_home = environment_value(environment, "VIA_HOME").map(PathBuf::from);
    let resolved = RuntimeHomeResolver::resolve(&inputs, &FilesystemProbe)?;
    Ok(json!({"ok":true,"observation":{"path":resolved.path()}}))
}

fn observe_runtime(input: &Value, environment: &[(&str, String)]) -> TestResult<Value> {
    let resolved = resolve_runtime(RuntimeResolutionInput {
        authored_runtime: None,
        runtime_policy_applied: true,
        forced_runtime: optional_go_non_empty(input, "forced_runtime"),
        environment_runtime: environment_value(environment, "NANIKA_DEFAULT_RUNTIME"),
        configured_tier_runtime: optional_go_non_empty(input, "configured_runtime"),
        policy_runtime: optional_go_non_empty(input, "policy_runtime"),
    });
    Ok(json!({"ok":true,"observation":{"runtime":resolved.runtime}}))
}

fn observe_model(input: &Value) -> TestResult<Value> {
    let mut model_tiers = BTreeMap::new();
    for (tier, value) in object(input, "routing")? {
        model_tiers.insert(
            tier.clone(),
            RoutingTier {
                provider: String::new(),
                model: string(value, "model")?.to_owned(),
                runtime: string(value, "runtime")?.to_owned(),
            },
        );
    }
    let forced = optional_go_non_empty(input, "forced_model");
    let tier = string(input, "tier")?;
    let effective_runtime = string(input, "effective_runtime")?;
    let model = resolve_model(ModelResolutionInput {
        forced_model: forced,
        tier: tier.to_owned(),
        effective_runtime: effective_runtime.to_owned(),
        routing_map: RoutingMap { model_tiers },
        built_in_model: string(input, "built_in_model")?.to_owned(),
    });
    Ok(json!({"ok":true,"observation":{"model":model}}))
}

fn observe_routing(input: &Value) -> TestResult<Value> {
    let present = input
        .get("present")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let run = input.get("run").and_then(Value::as_bool).unwrap_or(false);
    let verbose = input
        .get("verbose")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !present {
        if run {
            return Ok(json!({"ok":true,"observation":{"model_tiers":{},"warning_emitted":false}}));
        }
        return Ok(json!({"ok":false,"error":"missing_routing"}));
    }
    let source = string(input, "bytes")?;
    match serde_saphyr::from_str::<RoutingMap>(source) {
        Ok(map) if run => Ok(
            json!({"ok":true,"observation":{"model_tiers":map.model_tiers,"warning_emitted":false}}),
        ),
        Ok(map) => Ok(json!({"ok":true,"observation":map})),
        Err(error) if run => {
            let (map, warning) =
                routing_map_for_run(Err(RoutingConfigError::Malformed(error)), verbose);
            Ok(
                json!({"ok":true,"observation":{"model_tiers":map.model_tiers,"warning_emitted":warning.is_some()}}),
            )
        }
        Err(_) => Ok(json!({"ok":false,"error":"malformed_routing"})),
    }
}

fn observe_stall(input: &Value, environment: &[(&str, String)]) -> TestResult<Value> {
    let phase_timeout =
        optional_non_empty(input, "phase_timeout").and_then(|value| parse_duration_fixture(&value));
    let result = resolve_stall_timeout(StallResolutionInput {
        phase_timeout,
        flag_value: optional_go_non_empty(input, "flag_value"),
        environment_value: environment_value(environment, "ORCHESTRATOR_STALL_TIMEOUT"),
    });
    let value = match result {
        Ok(value) => value,
        Err(orchestrator_core::ConfigError::InvalidStallFlag { .. }) => {
            return Ok(json!({"ok":false,"error":"invalid_stall_flag"}));
        }
        Err(orchestrator_core::ConfigError::InvalidStallEnvironment { .. }) => {
            return Ok(json!({"ok":false,"error":"invalid_stall_environment"}));
        }
    };
    let (duration, worker_default) = match value.value {
        StallTimeoutValue::Duration(value) => (u64::try_from(value.as_nanos())?, Value::Null),
        StallTimeoutValue::WorkerDefault => (
            0,
            Value::String(string(input, "worker_default")?.to_owned()),
        ),
    };
    let mut observation = json!({"duration_ns":duration,"source":stall_source(value.source)});
    if !worker_default.is_null() {
        observation["worker_default"] = worker_default;
    }
    Ok(json!({"ok":true,"observation":observation}))
}

fn observe_checkpoint(input: &Value) -> TestResult<Value> {
    let decoded = match decode_checkpoint(string(input, "bytes")?.as_bytes()) {
        Ok(value) => value,
        Err(_) => return Ok(json!({"ok":false,"error":"unsupported_checkpoint"})),
    };
    let projection = &decoded.projection;
    let plan = projection.plan.as_ref().map(observe_checkpoint_plan);
    Ok(json!({"ok":true,"observation":{
        "version": checkpoint_payload_version(input), "workspace_id":projection.workspace_id,
        "domain":projection.domain,"status":projection.status,
        "started_at":zero_time(&projection.started_at),"git_repo_root":projection.git_repo_root,
        "worktree_path":projection.worktree_path,"branch_name":projection.branch_name,
        "base_branch":projection.base_branch,"plan":plan
    }}))
}

fn observe_checkpoint_plan(plan: &CheckpointPlan) -> Value {
    let phases = plan
        .phases
        .iter()
        .map(|phase| {
            json!({
                "id":phase.id,"name":phase.name,"objective":phase.objective,"persona":phase.persona,
                "model_tier":phase.model_tier,"skills":phase.skills,"constraints":phase.constraints,
                "dependencies":phase.dependencies,"expected":phase.expected,"status":phase.status,
                "role":phase.role,"target_dir":phase.target_dir,
                "priority":phase.priority,"runtime":phase.runtime,
                "runtime_policy_applied":phase.runtime_policy_applied,
                "stall_timeout":phase.stall_timeout
            })
        })
        .collect::<Vec<_>>();
    json!({"id":plan.id,"task":plan.task,"phases":phases,"execution_mode":plan.execution_mode,"decomp_source":plan.decomp_source,"created_at":zero_time(&plan.created_at)})
}

fn checkpoint_payload_version(input: &Value) -> i64 {
    let parsed: Value =
        serde_json::from_str(input["bytes"].as_str().unwrap_or_default()).unwrap_or(Value::Null);
    parsed
        .get("workspace_id")
        .and_then(Value::as_str)
        .map_or_else(
            || parsed["payload"]["version"].as_i64().unwrap_or_default(),
            |_| parsed["version"].as_i64().unwrap_or_default(),
        )
}

fn observe_events(input: &Value) -> TestResult<Value> {
    let scan = scan_event_log(string(input, "bytes")?.as_bytes());
    Ok(json!({"ok":true,"observation":{"next_sequence":scan.next_sequence.unwrap_or(1)}}))
}

fn observe_projection(input: &Value) -> TestResult<Value> {
    let definitions = input["definitions"]
        .as_array()
        .ok_or("definitions must be an array")?;
    let mut names = BTreeMap::new();
    let mut phases = Vec::new();
    for definition in definitions {
        let id = PhaseId::new(string(definition, "id")?)?;
        names.insert(id.clone(), string(definition, "name")?.to_owned());
        phases.push(PhaseDefinition {
            id,
            dependencies: string_array(definition, "dependencies")?
                .into_iter()
                .map(PhaseId::new)
                .collect::<Result<Vec<_>, _>>()?,
        });
    }
    let mission_id = MissionId::new(string(input, "mission_id")?)?;
    let mut state = MissionState::new(mission_id.clone(), phases)?;
    for event in input["events"]
        .as_array()
        .ok_or("events must be an array")?
    {
        let bytes = serde_json::to_vec(event)?;
        let decoded = orchestrator_core::decode_event_line(&bytes)?;
        let reducer_input = project_event(&decoded)?;
        let replay = state.applied_event(&reducer_input.event_id).is_some();
        let before = replay.then(|| state.clone());
        match reduce(&state, &reducer_input) {
            Ok(reduction) => {
                if before
                    .as_ref()
                    .is_some_and(|before| before != &reduction.state)
                {
                    return Ok(json!({"ok":false,"error":"replay_mutated_state"}));
                }
                state = reduction.state;
            }
            Err(orchestrator_core::TransitionError::OutOfOrder { .. }) => {
                return Ok(json!({"ok":false,"error":"out_of_order"}));
            }
            Err(error) => return Ok(json!({"ok":false,"error":format!("transition:{error}")})),
        }
    }
    let phases = state.phases().map(|phase| json!({
        "id":phase.id.as_str(),"name":names.get(&phase.id).cloned().unwrap_or_default(),
        "status":phase_status(phase.status),"started_at":phase.started_at.clone().unwrap_or_default(),
        "ended_at":phase.finished_at.clone().unwrap_or_default()
    })).collect::<Vec<_>>();
    Ok(
        json!({"ok":true,"observation":{"mission_id":mission_id.as_str(),"status":mission_status(state.status()),"started_at":state.started_at().unwrap_or_default(),"ended_at":state.finished_at().unwrap_or_default(),"phases":phases}}),
    )
}

struct FilesystemProbe;
impl DirectoryProbe for FilesystemProbe {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

fn require_success(context: &str, output: &Output) -> TestResult {
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{context} failed with {}:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}

fn tree_manifest(root: &Path) -> TestResult<Vec<(PathBuf, Vec<u8>, bool)>> {
    fn walk(root: &Path, path: &Path, result: &mut Vec<(PathBuf, Vec<u8>, bool)>) -> TestResult {
        let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            let contents = if metadata.is_file() {
                fs::read(&path)?
            } else if metadata.file_type().is_symlink() {
                fs::read_link(&path)?
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec()
            } else {
                Vec::new()
            };
            result.push((
                path.strip_prefix(root)?.to_path_buf(),
                contents,
                metadata.is_dir(),
            ));
            if metadata.is_dir() {
                walk(root, &path, result)?;
            }
        }
        Ok(())
    }
    let mut result = Vec::new();
    walk(root, root, &mut result)?;
    Ok(result)
}

fn assert_no_socket_or_database_artifacts(root: &Path) -> TestResult {
    #[cfg(unix)]
    use std::os::unix::fs::FileTypeExt;
    fn walk(path: &Path) -> TestResult {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
            if metadata.is_dir()
                && name == "build"
                && path
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|parent| parent == "fixture")
            {
                continue;
            }
            #[cfg(unix)]
            if metadata.file_type().is_socket() {
                return Err(format!("socket artifact survived: {path:?}").into());
            }
            if metadata.is_dir() {
                walk(&path)?;
            }
            if name.ends_with("-wal")
                || name.ends_with("-shm")
                || name.ends_with("-journal")
                || name.ends_with(".db")
                || name.ends_with(".sqlite")
                || name == "checkpoint.json.tmp"
                || name == "events.jsonl.tmp"
            {
                return Err(format!("forbidden fixture artifact survived: {path:?}").into());
            }
        }
        Ok(())
    }
    walk(root)
}

fn assert_no_surviving_fixture_processes(fixture: &Path) -> TestResult {
    let ps = if Path::new("/bin/ps").is_file() {
        "/bin/ps"
    } else {
        "/usr/bin/ps"
    };
    let output = Command::new(ps)
        .args(["-axo", "command="])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()?;
    require_success("process survivor sentinel", &output)?;
    let needle = fixture.to_string_lossy();
    let process_list = String::from_utf8_lossy(&output.stdout);
    let survivors = process_list
        .lines()
        .filter(|line| line.contains(needle.as_ref()))
        .collect::<Vec<_>>();
    if survivors.is_empty() {
        Ok(())
    } else {
        Err(format!("fixture subprocesses survived:\n{}", survivors.join("\n")).into())
    }
}

fn diff_error(case_id: &str, go: &Value, rust: &Value) -> String {
    format!(
        "differential mismatch for {case_id}\n--- go/expected\n{}\n+++ rust/actual\n{}",
        pretty(go),
        pretty(rust)
    )
}
fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}
fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}
fn fixture_case_path(fixture: &Path, case_id: &str) -> TestResult<PathBuf> {
    let relative = Path::new(case_id);
    if !safe_relative(relative) {
        return Err(format!("unsafe differential case id {case_id:?}").into());
    }
    Ok(fixture.join("cases").join(relative))
}
fn expand_fixture(value: &str, root: &Path) -> String {
    value.replace("$FIXTURE_ROOT", &root.to_string_lossy())
}
fn string<'a>(value: &'a Value, name: &str) -> TestResult<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{name} must be a string").into())
}
fn object<'a>(value: &'a Value, name: &str) -> TestResult<&'a Map<String, Value>> {
    value
        .get(name)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{name} must be an object").into())
}
fn string_array(value: &Value, name: &str) -> TestResult<Vec<String>> {
    value
        .get(name)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{name} must be an array"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{name} entries must be strings").into())
        })
        .collect()
}
fn optional_non_empty(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}
fn optional_go_non_empty(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
fn environment_value(environment: &[(&str, String)], name: &str) -> Option<String> {
    environment.iter().find_map(|(candidate, value)| {
        (*candidate == name && !value.is_empty()).then(|| value.clone())
    })
}
fn execution_mode(value: ExecutionMode) -> &'static str {
    match value {
        ExecutionMode::Sequential => "sequential",
        ExecutionMode::Parallel => "parallel",
    }
}
fn stall_source(value: StallSource) -> &'static str {
    match value {
        StallSource::Phase => "phase",
        StallSource::Flag => "flag",
        StallSource::Environment => "environment",
        StallSource::WorkerDefault => "worker_default",
    }
}
fn mission_status(value: orchestrator_core::MissionStatus) -> &'static str {
    match value {
        orchestrator_core::MissionStatus::NotStarted => "not_started",
        orchestrator_core::MissionStatus::InProgress => "in_progress",
        orchestrator_core::MissionStatus::Completed => "completed",
        orchestrator_core::MissionStatus::Failed => "failed",
        orchestrator_core::MissionStatus::Cancelled => "cancelled",
    }
}
fn phase_status(value: orchestrator_core::PhaseStatus) -> &'static str {
    match value {
        orchestrator_core::PhaseStatus::Pending => "pending",
        orchestrator_core::PhaseStatus::Running => "running",
        orchestrator_core::PhaseStatus::Completed => "completed",
        orchestrator_core::PhaseStatus::Failed => "failed",
        orchestrator_core::PhaseStatus::Skipped => "skipped",
    }
}
fn zero_time(value: &str) -> &str {
    value
}
fn parse_duration_fixture(value: &str) -> Option<Duration> {
    match value {
        "1m30s" => Some(Duration::from_secs(90)),
        _ => None,
    }
}
