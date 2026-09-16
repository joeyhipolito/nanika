use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orchestrator_exec::{
    Cancellation, Clock, EffectBudget, EffectRequest, EffectService, EffectServiceError,
    EffectServiceErrorKind, Effort, EventReceipt, EventSink, EventSinkError, EventSinkErrorKind,
    ExecutionRequestDraft, ExecutorRegistry, ProcessBudget, ProcessExitStatus, ProcessService,
    ProcessServiceError, RuntimeCap, WatchdogDecision, WatchdogPolicy, WorkerEventDraft,
    WorkerEventError, WorkerIdentity,
};
use serde::Deserialize;

use super::*;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const FROZEN_ORACLE: &str = include_str!("../tests/fixtures/go-sdk-one-shot-v1.json");
const RUST_STRICT_ORACLE: &str = include_str!("../tests/fixtures/rust-strict-protocol-v1.json");
// B5-DESIGN 9.F.4. This crate used to read the ten repository sources it was
// ported from with `include_bytes!`/`include_str!` five levels up, out of the
// Rust workspace. The verification lease archives exactly
// "$commit:skills/orchestrator-rs", so those paths are absent by construction
// and the crate could not compile inside the snapshot at all.
//
// The pins, the content anchors and the recorded envelopes now live in this
// in-workspace fixture, and `tests/reviewed-source-provenance.sh` asserts them
// against the live repository from `$repo`, where the sources exist: every pin
// by `git rev-parse "${commit}:<path>"`, every anchor by its presence or
// absence at that commit, and every record below by verbatim occurrence in the
// live capture. Neither half proves the claim alone; together they prove what
// the embed proved, in the place each input actually lives.
const PROVENANCE: &str = include_str!("../tests/fixtures/reviewed-go-sources-v1.json");
const PROVIDER_SOURCE: &str = include_str!("lib.rs");
const PRODUCTION_INIT: &str = concat!(
    r#"{"type":"system","subtype":"init","tools":[],"mcp_servers":[],"slash_commands":[],"agents":[],"skills":[],"plugins":[],"claude_code_version":"2.1.211","permissionMode":"dontAsk","output_style":"default","capabilities":[],"fast_mode_state":"off"}"#,
    "\n"
);
const PINNED_CLAUDE_CODE_SHA256_HEX: &str =
    "5a728a76198b6eca7f3c7cdbff43bab44b77b48c2108f7a3107d889773382629";

#[derive(Deserialize)]
struct Oracle {
    schema_version: u64,
    oracle_scope: String,
    source_commit: String,
    sources: Vec<OracleSource>,
    argv_cases: Vec<ArgvCase>,
    parser_cases: Vec<ParserCase>,
}

#[derive(Deserialize)]
struct StrictOracle {
    schema_version: u64,
    oracle_scope: String,
    go_fixture: String,
    parser_cases: Vec<ParserCase>,
}

#[derive(Deserialize)]
struct OracleSource {
    path: String,
    git_blob: String,
    // Present only in reviewed-go-sources-v1.json; the frozen Go oracle carries
    // the pins alone. The driver asserts these against the live sources.
    #[serde(default)]
    anchors_present: Vec<String>,
    #[serde(default)]
    anchors_absent: Vec<String>,
}

#[derive(Deserialize)]
struct ArgvCase {
    name: String,
    options: FixtureOptions,
    expected: Vec<String>,
}

#[derive(Deserialize)]
struct FixtureOptions {
    model: String,
    max_turns: u64,
    append_system_prompt: Option<String>,
    system_prompt: Option<String>,
    effort: String,
    disable_builtin_tools: bool,
    disable_mcp: bool,
    add_directory: Option<String>,
}

#[derive(Deserialize)]
struct ParserCase {
    name: String,
    stdout: String,
    expected_output: Option<String>,
    expected_error: Option<String>,
    expected_cost: Option<FixtureCost>,
}

#[derive(Deserialize)]
struct FixtureCost {
    input_tokens: u64,
    output_tokens: u64,
    total_cost_usd: f64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
}

#[derive(Deserialize)]
struct Provenance {
    schema_version: u64,
    source_commit: String,
    sources: Vec<OracleSource>,
    capture: ProvenanceCapture,
}

#[derive(Deserialize)]
struct ProvenanceCapture {
    path: String,
    shape: ProvenanceCaptureShape,
    records: Vec<CapturedCliRecord>,
}

#[derive(Deserialize)]
struct ProvenanceCaptureShape {
    system_subtypes: Vec<String>,
}

#[derive(Deserialize)]
struct CapturedCliRecord {
    name: String,
    raw: String,
}

fn oracle() -> Result<Oracle, serde_json::Error> {
    serde_json::from_str(FROZEN_ORACLE)
}

fn strict_oracle() -> Result<StrictOracle, serde_json::Error> {
    serde_json::from_str(RUST_STRICT_ORACLE)
}

fn provenance() -> Result<Provenance, serde_json::Error> {
    serde_json::from_str(PROVENANCE)
}

/// One recorded envelope by name, so a fixture that loses a record fails on the
/// missing name rather than by silently skipping the case it was carrying.
fn captured<'a>(fixture: &'a Provenance, name: &str) -> TestResult<&'a str> {
    fixture
        .capture
        .records
        .iter()
        .find(|record| record.name == name)
        .map(|record| record.raw.as_str())
        .ok_or_else(|| std::io::Error::other(format!("captured record {name} missing")).into())
}

#[test]
fn frozen_oracle_is_bound_to_reviewed_go_sources() -> TestResult {
    let fixture = oracle()?;
    assert_eq!(fixture.schema_version, 1);
    assert!(fixture.oracle_scope.contains("frozen Go SDK"));
    assert!(!fixture.oracle_scope.contains("Rust"));
    assert_eq!(
        fixture.source_commit,
        "f39aefcc48610be6040fc686b3a72188948bcbab"
    );
    assert_eq!(fixture.sources.len(), 10);
    let observed_sources = fixture
        .sources
        .iter()
        .map(|source| (source.path.as_str(), source.git_blob.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        observed_sources,
        vec![
            (
                "shared/sdk/query.go",
                "cf643daaab549905f420c7f1d198933f8988d8cd"
            ),
            (
                "shared/sdk/transport.go",
                "6d800f89775fb4a4206fb29580fb201d74a4f5da"
            ),
            (
                "shared/sdk/query_append_effort_test.go",
                "cc302698c14909a35273c66b7cc9ece072f99a3a"
            ),
            (
                "shared/sdk/query_disable_mcp_test.go",
                "0e0f3f8465513fcb90ed8c93918cb8243efa9836"
            ),
            (
                "shared/sdk/query_test.go",
                "29e9f57004ccd597bb11324669a384c269fdea4d"
            ),
            (
                "shared/sdk/env_allowlist_test.go",
                "6ad44fbb266be6f6438c406eecd135802b936ef4"
            ),
            (
                "shared/sdk/types.go",
                "5977bc077b72807398c2e221014c1fbde653fad9"
            ),
            (
                "skills/orchestrator/internal/worker/execute.go",
                "e07780f43e6127a6f4541e7277fb404f9aa55204"
            ),
            (
                "skills/orchestrator/internal/worker/execute_test.go",
                "5d6516c556bb67a45553feae0f8afab8b965c303"
            ),
            (
                "cmd/nanika/internal/tui/shell/testdata/claude-control-protocol/claude-allow.jsonl",
                "2e00def0a99cea19591980718bb6a315cbfe2f1b"
            ),
        ]
    );
    for source in &fixture.sources {
        assert!(!source.path.is_empty());
        assert_eq!(source.git_blob.len(), 40);
        assert!(source.git_blob.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
    // The same ten pins, in the same order, in the fixture the $repo-rooted
    // driver reads. Bytes are not read here: the driver resolves each path at
    // the tested commit with `git rev-parse` and fails on a missing path or a
    // moved blob id. This half proves the frozen oracle and the provenance
    // fixture name one and the same reviewed source list.
    let provenance = provenance()?;
    assert_eq!(provenance.schema_version, 1);
    assert_eq!(provenance.source_commit, fixture.source_commit);
    assert_eq!(
        provenance
            .sources
            .iter()
            .map(|source| (source.path.as_str(), source.git_blob.as_str()))
            .collect::<Vec<_>>(),
        observed_sources
    );
    assert_eq!(
        provenance.capture.path,
        "cmd/nanika/internal/tui/shell/testdata/claude-control-protocol/claude-allow.jsonl"
    );
    assert!(
        provenance
            .sources
            .iter()
            .any(|source| source.path == provenance.capture.path),
        "the recorded capture is not one of the pinned sources"
    );

    // TRK-1184: the Go SDK rewrite at and after 30ef5319 deleted the one-shot
    // path's cumulative stdout budget (queryStdoutMaxBytes) and its per-line
    // budget (queryMaxLineBytes) along with malformed-frame-is-terminal
    // semantics. Per the recorded supervisor decision narrowing the phase-1
    // HARD STOP to protocol records only, this is a tracked Go-side regression
    // (TRK-1184), not a Rust contract to relax: the Rust decoder deliberately
    // keeps enforcing both bounds as product resource policy.
    //
    // The Go direction of that divergence is now two `anchors_absent` entries
    // on shared/sdk/query.go in the provenance fixture, asserted by the driver
    // against the live file -- a Go-side restore of either constant still fails
    // loudly and forces re-anchoring. Assert here that the fixture has not
    // quietly dropped them, so the negative anchors cannot be lost by editing
    // the fixture alone.
    let query_go = provenance
        .sources
        .iter()
        .find(|source| source.path == "shared/sdk/query.go")
        .ok_or_else(|| std::io::Error::other("shared/sdk/query.go pin missing"))?;
    assert_eq!(
        query_go.anchors_absent,
        vec![
            "queryStdoutMaxBytes".to_owned(),
            "queryMaxLineBytes".to_owned()
        ]
    );
    assert!(query_go.anchors_present.len() >= 6);

    // Rust direction of the same TRK-1184 divergence: the resource-policy
    // bounds the Go one-shot path abandoned must still hold here.
    assert_eq!(MAX_STDOUT_BYTES, 32 * 1024 * 1024);
    assert_eq!(MAX_JSONL_LINE_BYTES, 10 * 1024 * 1024);
    Ok(())
}

#[test]
fn recorded_claude_cli_assistant_envelopes_match_closed_decoder() -> TestResult {
    let fixture = provenance()?;
    let thinking = captured(&fixture, "assistant_thinking")?;
    let text = captured(&fixture, "assistant_text")?;
    let tool = captured(&fixture, "assistant_tool_use")?;
    for (name, raw) in [
        ("assistant_thinking", thinking),
        ("assistant_text", text),
        ("assistant_tool_use", tool),
    ] {
        assert!(
            raw.contains(r#""type":"assistant""#),
            "recorded {name} is not an assistant envelope"
        );
    }

    let thinking_output = format!("{thinking}\n{{\"type\":\"result\",\"subtype\":\"success\"}}\n");
    assert!(matches!(
        parse_legacy_one_shot_output(thinking_output.as_bytes()),
        Err(ClaudeOutputError::EmptyOutput)
    ));

    let text_output = format!("{text}\n{{\"type\":\"result\",\"subtype\":\"success\"}}\n");
    let parsed = parse_legacy_one_shot_output(text_output.as_bytes())?;
    assert!(!parsed.output.is_empty());

    assert!(matches!(
        parse_legacy_one_shot_output(format!("{tool}\n").as_bytes()),
        Err(ClaudeOutputError::ToolProtocol)
    ));
    Ok(())
}

#[test]
fn recorded_claude_cli_system_and_result_envelopes_match_closed_decoder() -> TestResult {
    // The capture holds one recorded envelope per observed system subtype; the
    // fixture carries one of each. That the live capture holds *exactly* these
    // four subtypes, and that its first hook_started precedes its init, are
    // whole-file claims an excerpt cannot make: the driver asserts both against
    // the live capture (B5-DESIGN 9.F.4.2). What is provable here is that the
    // fixture still replays every subtype the driver pins, so the two halves
    // cannot drift apart silently.
    let fixture = provenance()?;
    let declared = &fixture.capture.shape.system_subtypes;
    assert_eq!(
        declared,
        &[
            "hook_response".to_owned(),
            "hook_started".to_owned(),
            "init".to_owned(),
            "thinking_tokens".to_owned()
        ]
    );

    let mut system_variants = std::collections::BTreeSet::new();
    for record in &fixture.capture.records {
        let envelope: serde_json::Value = serde_json::from_str(&record.raw)?;
        if envelope.get("type").and_then(serde_json::Value::as_str) != Some("system") {
            continue;
        }
        let subtype = envelope
            .get("subtype")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| std::io::Error::other("system subtype missing"))?;
        assert_eq!(
            record.name,
            format!("system_{subtype}"),
            "recorded system envelope is filed under the wrong name"
        );
        system_variants.insert(subtype.to_owned());
        let mut parser = OutputParser::new(ClaudeProtocolContract::legacy_oracle());
        let observed = parser.consume_system(&record.raw);
        match subtype {
            "thinking_tokens" => assert_eq!(observed, Ok(())),
            "init" | "hook_started" | "hook_response" => {
                assert_eq!(observed, Err(ClaudeOutputError::UnisolatedRuntime));
            }
            _ => {
                return Err(std::io::Error::other("unexpected captured system subtype").into());
            }
        }
    }
    assert_eq!(
        system_variants,
        declared.iter().cloned().collect(),
        "the replayed subtypes are not the subtypes the driver pins"
    );

    let captured_result = captured(&fixture, "result")?;
    let _: ResultWire = serde_json::from_str(captured_result)
        .map_err(|error| std::io::Error::other(format!("captured result decode: {error}")))?;
    let mut parser = OutputParser::new(ClaudeProtocolContract::legacy_oracle());
    parser.output = "captured".to_owned();
    parser.turn_state = TurnState::AwaitingSuccessfulResult;
    parser.consume_line(captured_result.as_bytes())?;
    let parsed = parser.finish()?;
    assert_eq!(parsed.output, "captured\n\n");
    let cost = parsed
        .cost
        .ok_or_else(|| std::io::Error::other("captured result cost missing"))?;
    assert_eq!(cost.input_tokens(), 63_934);
    assert_eq!(cost.output_tokens(), 413);
    assert_eq!(cost.cache_creation_tokens(), 12_101);
    assert_eq!(cost.cache_read_tokens(), 51_815);
    assert_eq!(cost.total_cost_usd(), 0.031_466_5);
    Ok(())
}

#[test]
fn frozen_argv_cases_match_go_ordering() -> TestResult {
    let fixture = oracle()?;
    for case in fixture.argv_cases {
        let options = GoOneShotOptions {
            model: &case.options.model,
            max_turns: case.options.max_turns,
            append_system_prompt: case.options.append_system_prompt.as_deref(),
            system_prompt: case.options.system_prompt.as_deref(),
            effort: &case.options.effort,
            disable_builtin_tools: case.options.disable_builtin_tools,
            disable_mcp: case.options.disable_mcp,
            add_directory: case.options.add_directory.as_deref(),
        };
        assert_eq!(
            build_one_shot_arguments(&options),
            case.expected,
            "argv fixture {}",
            case.name
        );
    }
    Ok(())
}

#[test]
fn frozen_parser_cases_match_bounded_oracle() -> TestResult {
    let fixture = oracle()?;
    for case in fixture.parser_cases {
        assert_parser_case(case);
    }
    Ok(())
}

#[test]
fn rust_only_protocol_cases_are_proven_separately_from_go_parity() -> TestResult {
    let fixture = strict_oracle()?;
    assert_eq!(fixture.schema_version, 1);
    assert!(fixture.oracle_scope.contains("Rust-only"));
    assert!(fixture.oracle_scope.contains("not Go parity evidence"));
    assert_eq!(fixture.go_fixture, "go-sdk-one-shot-v1.json");
    assert_eq!(fixture.parser_cases.len(), 50);
    let unique_names = fixture
        .parser_cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique_names.len(), fixture.parser_cases.len());
    for case in fixture.parser_cases {
        assert_parser_case(case);
    }
    Ok(())
}

#[test]
fn production_protocol_requires_exact_isolated_init_before_output() -> TestResult {
    let stdout = [
        PRODUCTION_INIT,
        "{\"type\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"isolated\"}]}\n",
        "{\"type\":\"result\",\"subtype\":\"success\"}\n",
    ]
    .concat();
    assert_eq!(
        parse_one_shot_output(stdout.as_bytes())?.output,
        "isolated\n\n"
    );

    let without_init = concat!(
        "{\"type\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"unsafe\"}]}\n",
        "{\"type\":\"result\",\"subtype\":\"success\"}\n"
    );
    assert!(matches!(
        parse_one_shot_output(without_init.as_bytes()),
        Err(ClaudeOutputError::UnisolatedRuntime)
    ));
    Ok(())
}

#[test]
fn pinned_build_identity_is_typed_without_digest_drift() -> TestResult {
    use std::fmt::Write as _;

    let mut digest = String::with_capacity(PINNED_CLAUDE_CODE_SHA256.len() * 2);
    for byte in PINNED_CLAUDE_CODE_SHA256 {
        write!(&mut digest, "{byte:02x}")?;
    }

    assert_eq!(PINNED_CLAUDE_CODE_VERSION, "2.1.211");
    assert_eq!(PINNED_CLAUDE_CODE_LENGTH_BYTES, 242_445_680);
    assert_eq!(digest, PINNED_CLAUDE_CODE_SHA256_HEX);
    Ok(())
}

#[test]
fn production_protocol_rejects_version_permission_hook_and_duplicate_init_drift() -> TestResult {
    let wrong_version = PRODUCTION_INIT.replace(
        r#""claude_code_version":"2.1.211""#,
        r#""claude_code_version":"2.1.212""#,
    );
    assert!(matches!(
        parse_one_shot_output(wrong_version.as_bytes()),
        Err(ClaudeOutputError::UnknownProtocolValue)
    ));

    let missing_version = PRODUCTION_INIT.replace(r#""claude_code_version":"2.1.211","#, "");
    assert!(matches!(
        parse_one_shot_output(missing_version.as_bytes()),
        Err(ClaudeOutputError::UnknownProtocolValue)
    ));

    for invalid_permission in [
        PRODUCTION_INIT.replace(
            r#""permissionMode":"dontAsk""#,
            r#""permissionMode":"default""#,
        ),
        PRODUCTION_INIT.replace(r#","permissionMode":"dontAsk""#, ""),
    ] {
        assert!(matches!(
            parse_one_shot_output(invalid_permission.as_bytes()),
            Err(ClaudeOutputError::UnisolatedRuntime)
        ));
    }

    for required_field in [
        "tools",
        "mcp_servers",
        "slash_commands",
        "agents",
        "skills",
        "plugins",
        "capabilities",
        "output_style",
        "fast_mode_state",
    ] {
        let mut init: serde_json::Value = serde_json::from_str(PRODUCTION_INIT.trim_end())?;
        init.as_object_mut()
            .ok_or_else(|| std::io::Error::other("production init fixture is not an object"))?
            .remove(required_field);
        let incomplete = format!("{init}\n");
        assert!(
            matches!(
                parse_one_shot_output(incomplete.as_bytes()),
                Err(ClaudeOutputError::UnisolatedRuntime)
            ),
            "missing {required_field}"
        );
    }

    let mut capability_init: serde_json::Value = serde_json::from_str(PRODUCTION_INIT.trim_end())?;
    capability_init["capabilities"] = serde_json::json!(["interrupt_receipt_v1"]);
    let capability_init = format!("{capability_init}\n");
    assert!(matches!(
        parse_one_shot_output(capability_init.as_bytes()),
        Err(ClaudeOutputError::UnisolatedRuntime)
    ));

    let mut output_style_init: serde_json::Value =
        serde_json::from_str(PRODUCTION_INIT.trim_end())?;
    output_style_init["output_style"] = serde_json::json!("custom");
    let output_style_init = format!("{output_style_init}\n");
    assert!(matches!(
        parse_one_shot_output(output_style_init.as_bytes()),
        Err(ClaudeOutputError::UnisolatedRuntime)
    ));

    let hook_before_init = concat!(
        "{\"type\":\"system\",\"subtype\":\"hook_started\",\"hook_id\":\"h\",",
        "\"hook_name\":\"fixture\",\"hook_event\":\"SessionStart\"}\n"
    );
    assert!(matches!(
        parse_one_shot_output(hook_before_init.as_bytes()),
        Err(ClaudeOutputError::UnisolatedRuntime)
    ));

    let duplicate_init = format!("{PRODUCTION_INIT}{PRODUCTION_INIT}");
    assert!(matches!(
        parse_one_shot_output(duplicate_init.as_bytes()),
        Err(ClaudeOutputError::Malformed)
    ));
    Ok(())
}

fn parse_legacy_one_shot_output(stdout: &[u8]) -> Result<ParsedOutput, ClaudeOutputError> {
    parse_one_shot_output_with_contract(stdout, ClaudeProtocolContract::legacy_oracle())
}

fn assert_parser_case(case: ParserCase) {
    match parse_legacy_one_shot_output(case.stdout.as_bytes()) {
        Ok(parsed) => {
            assert_eq!(case.expected_error, None, "parser fixture {}", case.name);
            assert_eq!(
                Some(parsed.output),
                case.expected_output,
                "parser fixture {}",
                case.name
            );
            assert_cost(
                parsed.cost.as_ref(),
                case.expected_cost.as_ref(),
                &case.name,
            );
        }
        Err(error) => {
            assert_eq!(case.expected_output, None, "parser fixture {}", case.name);
            assert_eq!(
                Some(format!("{error:?}")),
                case.expected_error,
                "parser fixture {}",
                case.name
            );
            assert!(case.expected_cost.is_none(), "parser fixture {}", case.name);
            assert!(!format!("{error:?}").contains("DO_NOT_RETAIN_ORACLE_SECRET"));
            assert!(!error.to_string().contains("DO_NOT_RETAIN_ORACLE_SECRET"));
        }
    }
}

fn assert_cost(actual: Option<&CostInfo>, expected: Option<&FixtureCost>, case_name: &str) {
    match (actual, expected) {
        (Some(actual), Some(expected)) => {
            assert_eq!(actual.input_tokens(), expected.input_tokens, "{case_name}");
            assert_eq!(
                actual.output_tokens(),
                expected.output_tokens,
                "{case_name}"
            );
            assert_eq!(
                actual.total_cost_usd(),
                expected.total_cost_usd,
                "{case_name}"
            );
            assert_eq!(
                actual.cache_creation_tokens(),
                expected.cache_creation_tokens,
                "{case_name}"
            );
            assert_eq!(
                actual.cache_read_tokens(),
                expected.cache_read_tokens,
                "{case_name}"
            );
        }
        (None, None) => {}
        (actual, expected) => assert_eq!(
            actual.is_some(),
            expected.is_some(),
            "cost mismatch for {case_name}"
        ),
    }
}

#[test]
fn parser_limits_are_cumulative_and_per_record() {
    let mut parser = OutputParser::new(ClaudeProtocolContract::legacy_oracle());
    assert_eq!(
        parser.charge_line(MAX_JSONL_LINE_BYTES),
        Err(ClaudeOutputError::LineLimit)
    );
    let mut parser = OutputParser::new(ClaudeProtocolContract::legacy_oracle());
    assert_eq!(parser.charge_line(MAX_JSONL_LINE_BYTES - 1), Ok(()));

    let mut parser = OutputParser::new(ClaudeProtocolContract::legacy_oracle());
    parser.charged_bytes = MAX_STDOUT_BYTES;
    assert_eq!(
        parser.charge_line(0),
        Err(ClaudeOutputError::CumulativeLimit)
    );
}

#[test]
fn prompt_framing_matches_go_and_truncates_on_utf8_boundary() -> TestResult {
    let prior = "é".repeat(4_001);
    let prompt = build_worker_prompt(&prior, "Summarize safely")?;
    assert!(prompt.starts_with(PROMPT_PREAMBLE));
    assert!(prompt.contains(PRIOR_CONTEXT_OPEN));
    assert!(prompt.contains("[Note: Prior context truncated; original was 8002 characters]"));
    assert!(prompt.ends_with("Task: Summarize safely"));
    assert!(prompt.len() <= MAX_ARGUMENT_BYTES);
    assert!(std::str::from_utf8(prompt.as_bytes()).is_ok());
    Ok(())
}

#[test]
fn eight_thousand_byte_prior_context_stays_inside_process_argument_contract() -> TestResult {
    let prior = "p".repeat(MAX_PRIOR_CONTEXT_BYTES);
    let objective = "required-task-text-".repeat(32);
    let provider = ClaudeProvider::new(ClaudeProviderConfig::new())?;
    let request = execution_request_with(objective.clone(), prior)?;
    let process = provider.process_request(&request)?;
    let prompt = process
        .expose_arguments()
        .last()
        .ok_or_else(|| std::io::Error::other("Claude prompt argument missing"))?;

    assert!(prompt.len() <= MAX_ARGUMENT_BYTES);
    assert!(prompt.contains("[Note: Prior context truncated; original was 8000 characters]"));
    assert!(prompt.ends_with(&format!("{TASK_PREFIX}{objective}")));
    Ok(())
}

#[test]
fn prompt_budget_preserves_the_complete_task_or_rejects_it() -> TestResult {
    let exact_objective = "x".repeat(MAX_ARGUMENT_BYTES - TASK_PREFIX.len());
    let exact = build_worker_prompt("optional prior context", &exact_objective)?;
    assert_eq!(exact.len(), MAX_ARGUMENT_BYTES);
    assert_eq!(exact, format!("{TASK_PREFIX}{exact_objective}"));

    let oversized_objective = format!("{exact_objective}x");
    assert_eq!(
        build_worker_prompt("optional prior context", &oversized_objective),
        Err(ClaudeRequestError::TaskExceedsArgumentBudget)
    );
    Ok(())
}

#[test]
fn configuration_is_explicit_allowlisted_and_redacted() -> TestResult {
    let allowed = [
        "HOME",
        "PATH",
        "LANG",
        "TERM",
        "USER",
        "SHELL",
        "TMPDIR",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CONFIG_DIR",
    ];
    let mut exact_allowlist = ClaudeProviderConfig::new();
    for key in allowed {
        exact_allowlist = exact_allowlist.with_environment(key, "fixture-value")?;
    }
    assert_eq!(exact_allowlist.environment.len(), allowed.len());

    let config = ClaudeProviderConfig::new()
        .with_environment("ANTHROPIC_AUTH_TOKEN", "super-secret-token")?
        .with_environment("PATH", "/fixture/bin")?
        .with_system_prompt("system-secret")?
        .with_persona_prompt("reviewer", "persona-secret")?;
    let debug = format!("{config:?}");
    for secret in ["super-secret-token", "system-secret", "persona-secret"] {
        assert!(!debug.contains(secret));
    }

    let error = ClaudeProviderConfig::new()
        .with_environment("SHOULD_NOT_LEAK", "ambient-secret")
        .err()
        .ok_or_else(|| std::io::Error::other("unknown environment key was accepted"))?;
    assert_eq!(error, ClaudeProviderConfigError::EnvironmentKeyNotAllowed);
    assert!(!format!("{error:?}").contains("ambient-secret"));
    assert!(!error.to_string().contains("ambient-secret"));
    assert_eq!(
        ClaudeProviderConfig::new()
            .with_environment(ENTRYPOINT_KEY, "caller-override")
            .err(),
        Some(ClaudeProviderConfigError::EnvironmentKeyNotAllowed)
    );
    Ok(())
}

#[test]
fn process_request_is_pinned_tool_less_and_extension_less() -> TestResult {
    let config = ClaudeProviderConfig::new()
        .with_environment("PATH", "/fixture/bin")?
        .with_environment("ANTHROPIC_AUTH_TOKEN", "token-secret")?
        .with_system_prompt("replacement-must-lose")?
        .with_persona_prompt("reviewer", "append-persona")?;
    let provider = ClaudeProvider::new(config)?;
    let request = execution_request()?;
    let process = provider.process_request(&request)?;

    assert_eq!(process.executable_id(), "claude");
    assert_eq!(
        process.working_root(),
        std::path::Path::new("/fixture/worker")
    );
    assert_eq!(process.max_output_bytes(), MAX_STDOUT_BYTES);
    assert!(!process.truncated_output_acknowledged());
    assert!(process.expose_stdin().is_none());
    let arguments = process.expose_arguments();
    let expected_prompt = format!(
        "{PROMPT_PREAMBLE}{PRIOR_CONTEXT_OPEN}prior evidence{PRIOR_CONTEXT_CLOSE}{TASK_PREFIX}Review the implementation"
    );
    let expected_arguments = [
        "--output-format",
        "stream-json",
        "--input-format",
        "text",
        "--print",
        "--verbose",
        "--include-partial-messages",
        "--include-hook-events",
        "--safe-mode",
        "--setting-sources",
        "",
        "--disable-slash-commands",
        "--no-chrome",
        "--no-session-persistence",
        "--permission-mode",
        "dontAsk",
        "--prompt-suggestions",
        "false",
        "--model",
        "claude-opus-4-8",
        "--max-turns",
        "20",
        "--append-system-prompt",
        "append-persona",
        "--effort",
        "high",
        "--tools",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        EMPTY_MCP_CONFIG,
        "-p",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(std::iter::once(expected_prompt))
    .collect::<Vec<_>>();
    assert_eq!(arguments, expected_arguments);
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == ["--append-system-prompt", "append-persona"])
    );
    assert!(
        !arguments
            .iter()
            .any(|argument| argument == "--system-prompt")
    );
    assert!(arguments.windows(2).any(|pair| pair == ["--tools", ""]));
    assert!(
        arguments
            .iter()
            .any(|argument| argument == "--strict-mcp-config")
    );
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == ["--mcp-config", EMPTY_MCP_CONFIG])
    );
    for flag in [
        "--include-hook-events",
        "--safe-mode",
        "--disable-slash-commands",
        "--no-chrome",
        "--no-session-persistence",
    ] {
        assert!(arguments.iter().any(|argument| argument == flag), "{flag}");
    }
    for pair in [
        ["--input-format", "text"],
        ["--setting-sources", ""],
        ["--permission-mode", "dontAsk"],
        ["--prompt-suggestions", "false"],
    ] {
        assert!(
            arguments.windows(2).any(|window| window == pair),
            "{pair:?}"
        );
    }
    for forbidden in [
        "--add-dir",
        "--resume",
        "--bare",
        "--dangerously-skip-permissions",
    ] {
        assert!(
            !arguments.iter().any(|argument| argument == forbidden),
            "{forbidden}"
        );
    }
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == ["--effort", "high"])
    );
    assert!(
        arguments
            .last()
            .is_some_and(|prompt| prompt.ends_with("Task: Review the implementation"))
    );

    let environment = process.expose_environment();
    assert!(environment.contains(&("PATH".to_owned(), "/fixture/bin".to_owned())));
    assert!(environment.contains(&("ANTHROPIC_AUTH_TOKEN".to_owned(), "token-secret".to_owned())));
    assert!(environment.contains(&(ENTRYPOINT_KEY.to_owned(), ENTRYPOINT_VALUE.to_owned())));
    assert!(
        !environment
            .iter()
            .any(|(key, _)| key == "CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD")
    );
    assert!(!environment.iter().any(|(key, _)| key == "SHOULD_NOT_LEAK"));
    assert!(
        !environment
            .iter()
            .any(|(key, _)| key == "CLAUDE_CODE_EFFORT_LEVEL")
    );
    Ok(())
}

#[test]
fn process_request_rejects_target_directory_authority() -> TestResult {
    let provider = ClaudeProvider::new(ClaudeProviderConfig::new())?;
    let request = execution_request_with_target(
        "Review the implementation".to_owned(),
        "prior evidence".to_owned(),
        Some("/fixture/target".into()),
    )?;
    assert!(matches!(
        provider.process_request(&request),
        Err(ClaudeRequestError::TargetDirectoryNotIsolated)
    ));
    Ok(())
}

#[test]
fn descriptor_advertises_only_proven_cost_capability() -> TestResult {
    let provider = ClaudeProvider::new(ClaudeProviderConfig::new())?;
    let descriptor = provider
        .descriptor()
        .ok_or_else(|| std::io::Error::other("Claude descriptor missing"))?;
    assert_eq!(descriptor.runtime().as_str(), "claude");
    assert!(descriptor.supports(RuntimeCap::CostReport));
    for capability in [
        RuntimeCap::ToolUse,
        RuntimeCap::SessionResume,
        RuntimeCap::Streaming,
        RuntimeCap::Artifacts,
    ] {
        assert!(!descriptor.supports(capability));
    }
    Ok(())
}

/// Recursively lists every `.rs` file under `dir`.
fn rust_source_files(dir: &std::path::Path) -> TestResult<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current)
            .map_err(|error| format!("read_dir {}: {error}", current.display()))?
        {
            let path = entry
                .map_err(|error| format!("dir entry under {}: {error}", current.display()))?
                .path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }
    Ok(files)
}

/// A handful of files in `orchestrator-app` are gated behind an inner
/// `#![cfg(... feature = "verification-process-canary" ...)]` at the top —
/// the whole file compiles only under the canary feature, so it is never
/// part of the ordinary run path regardless of what it names.
fn is_whole_file_canary_gated(source: &str) -> bool {
    source
        .lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|first_line| {
            let trimmed = first_line.trim_start();
            trimmed.starts_with("#![cfg(") && trimmed.contains("verification-process-canary")
        })
}

/// Scans forward from `text[0]`, skipping Rust line/block comments and
/// string/char literals, and reports the byte index at which every opened
/// `(`, `[` and `{` delimiter has been closed again (`depth` returns to 0)
/// while `predicate` accepts the closing/terminating character seen at that
/// point. Shared by the attribute skipper and the item skipper below so both
/// agree on what counts as "inside a nested scope" (and therefore not a real
/// terminator).
fn scan_balanced(
    text: &str,
    mut is_terminator: impl FnMut(char, i32, bool) -> bool,
) -> Option<usize> {
    let mut depth: i32 = 0;
    let mut seen_brace = false;
    let mut chars = text.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        match ch {
            '/' if matches!(chars.peek(), Some((_, '/'))) => {
                for (_, c) in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '/' if matches!(chars.peek(), Some((_, '*'))) => {
                chars.next();
                let mut prev = '\0';
                for (_, c) in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
            }
            '"' => {
                let mut escaped = false;
                for (_, c) in chars.by_ref() {
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        break;
                    }
                }
            }
            '\'' => {
                // A Rust lifetime (`'a`) also starts with a single quote but
                // is never followed by a closing quote; only consume this as
                // a char literal if it actually closes.
                let mut lookahead = chars.clone();
                let mut escaped = false;
                let mut consumed = Vec::new();
                let mut closed = false;
                for item in lookahead.by_ref() {
                    consumed.push(item);
                    let (_, c) = item;
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '\'' {
                        closed = true;
                        break;
                    } else if c == '\n' {
                        break;
                    }
                }
                if closed {
                    chars = lookahead;
                }
            }
            '(' | '[' | '{' => {
                depth += 1;
                if ch == '{' {
                    seen_brace = true;
                }
            }
            ')' | ']' | '}' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
                if depth == 0 && is_terminator(ch, depth, seen_brace) {
                    return Some(idx + ch.len_utf8());
                }
            }
            ';' | ',' if depth == 0 && is_terminator(ch, depth, seen_brace) => {
                return Some(idx + ch.len_utf8());
            }
            _ => {}
        }
    }
    None
}

/// `text` starts with an attribute's leading `#`. Returns the byte index
/// just past that attribute's closing `]` (e.g. `#[error("...")]`).
fn skip_attribute(text: &str) -> Option<usize> {
    if !text.starts_with('#') {
        return None;
    }
    let after_hash = &text[1..];
    if !after_hash.starts_with('[') {
        return None;
    }
    let end = scan_balanced(after_hash, |ch, depth, _seen_brace| depth == 0 && ch == ']')?;
    Some(1 + end)
}

/// `text` starts right after an attribute (and any attributes stacked on
/// top of it). Returns the byte index just past the single item that
/// attribute gates: a brace-delimited item (fn/mod/impl/struct/enum/block)
/// ends at the matching `}`; a semicolon- or comma-terminated item (`use`,
/// `type`, a struct field, an enum variant, a const) ends at the first `;`
/// or `,` seen before any `{` opens.
fn skip_gated_item(text: &str) -> Option<usize> {
    scan_balanced(text, |ch, depth, seen_brace| {
        depth == 0 && (ch == '}' || ((ch == ';' || ch == ',') && !seen_brace))
    })
}

/// Returns the portion of `source` compiled into the ordinary (non-test)
/// build: `source` with every `#[cfg(test)]`-gated item removed, on the
/// convention already established at
/// `orchestrator_app::worker_spawn::production_source_holds_exactly_one_routing_decision_call_site`.
/// Earlier revisions of this helper only stripped the text after the FIRST
/// `#[cfg(test)]` marker, which silently skipped scanning any production
/// code that came after an earlier, non-trailing marker (e.g. a
/// `#[cfg(test)] use ...;` near the top of a file, or `#[cfg(test)]`-gated
/// struct fields threaded through production `impl` blocks) — real
/// production code has since accumulated in exactly that shape. This walks
/// every marker in the file and removes only the single item each one
/// gates, so text before, between, and after every marker is scanned.
fn production_source(source: &str) -> Result<String, String> {
    let marker = "#[cfg(test)]";
    let mut result = String::with_capacity(source.len());
    let mut cursor = 0usize;
    while let Some(relative) = source[cursor..].find(marker) {
        let marker_start = cursor + relative;
        result.push_str(&source[cursor..marker_start]);
        let mut pos = marker_start;
        loop {
            let rest = &source[pos..];
            let trimmed = rest.trim_start();
            let leading_ws = rest.len() - trimmed.len();
            match skip_attribute(trimmed) {
                Some(attr_len) => pos += leading_ws + attr_len,
                None => {
                    pos += leading_ws;
                    break;
                }
            }
        }
        let item_len = skip_gated_item(&source[pos..]).ok_or_else(|| {
            format!(
                "could not determine the end of the #[cfg(test)]-gated item starting at byte {pos}"
            )
        })?;
        cursor = pos + item_len;
    }
    result.push_str(&source[cursor..]);
    Ok(result)
}

/// TRK-1247: the prior version of this test only checked that
/// `orchestrator-app/Cargo.toml` did not name `orchestrator-provider-claude`
/// or `ClaudeProvider` as text. That is not the enrollment boundary — the
/// crate *is* an ordinary `orchestrator-app` dependency (for
/// `decode_claude_statusline_usage` / `ClaudeStatuslineQuotaObservationV1`
/// status-line accounting), so the manifest legitimately contains both
/// strings and the old assertions failed on a boundary that was never real.
///
/// The actual boundary is that no non-test, non-canary-feature source in
/// `orchestrator-app` or `orchestrator-cli` ever names the `ClaudeProvider`
/// type, i.e. nothing in the ordinary run path constructs one or hands it to
/// an `ExecutorRegistry`. This walks every `.rs` file in both crates' `src/`
/// trees, skips whole-file `verification-process-canary`-gated files
/// ([`is_whole_file_canary_gated`]), strips every `#[cfg(test)]`-gated item
/// out of each remaining file ([`production_source`]), and proves
/// `ClaudeProvider` is absent from what remains — a file may carry any
/// number of `#[cfg(test)]` markers, not just a single trailing one.
#[test]
fn production_enrollment_stays_blocked_until_subscription_isolation_trap_passes() -> TestResult {
    let provider = ClaudeProvider::new(ClaudeProviderConfig::new())?;
    assert_eq!(
        provider.enrollment_status(),
        ClaudeEnrollmentStatus::BlockedUntilSubscriptionIsolationTrapPasses
    );

    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for sibling_crate in ["orchestrator-app", "orchestrator-cli"] {
        let src_dir = manifest_dir.join("..").join(sibling_crate).join("src");
        for source_file in rust_source_files(&src_dir)? {
            let source = std::fs::read_to_string(&source_file)
                .map_err(|error| format!("read {}: {error}", source_file.display()))?;
            if is_whole_file_canary_gated(&source) {
                continue;
            }
            let scanned = production_source(&source)
                .map_err(|error| format!("{}: {error}", source_file.display()))?;
            assert!(
                !scanned.contains("ClaudeProvider"),
                "{} names ClaudeProvider outside #[cfg(test)]/canary-feature code, which \
                 would enroll the Claude provider into the ordinary run path (TRK-1247)",
                source_file.display()
            );
        }
    }
    Ok(())
}

/// A source with two `#[cfg(test)]` markers used to be truncated at the
/// first marker, silently dropping every production item after it —
/// including `fn c()` here, which sits between the two markers, and the
/// trailing `const AFTER: ...` after the second one. This proves the whole
/// file is scanned: production text on both sides of both markers survives,
/// while the two gated items (and only those) are removed.
#[test]
fn production_source_fully_scans_a_two_marker_file() -> TestResult {
    let two_marker_source = r#"fn a() { ClaudeProviderMarkerA; }
#[cfg(test)]
fn b_test_only() { ClaudeProviderDecoyB; }
fn c() { ClaudeProviderMarkerC; }
#[cfg(test)]
mod tests {
    fn nested() { ClaudeProviderDecoyD; }
}
const AFTER: &str = "ClaudeProviderMarkerE";
"#;

    let scanned = production_source(two_marker_source)?;

    for marker in [
        "ClaudeProviderMarkerA",
        "ClaudeProviderMarkerC",
        "ClaudeProviderMarkerE",
    ] {
        assert!(
            scanned.contains(marker),
            "production text {marker} was dropped from the scan: {scanned:?}"
        );
    }
    for decoy in ["ClaudeProviderDecoyB", "ClaudeProviderDecoyD"] {
        assert!(
            !scanned.contains(decoy),
            "gated test-only text {decoy} leaked into the production scan: {scanned:?}"
        );
    }
    Ok(())
}

#[test]
fn executor_uses_injected_process_service_and_returns_proven_cost() -> TestResult {
    let stdout = [
        PRODUCTION_INIT,
        "{\"type\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}\n",
        concat!(
            "{\"type\":\"result\",\"subtype\":\"success\",\"total_cost_usd\":0.5,",
            "\"usage\":{\"input_tokens\":2,\"cache_creation_input_tokens\":3,",
            "\"cache_read_input_tokens\":4,\"output_tokens\":5}}\n"
        ),
    ]
    .concat();
    let harness = Harness::new(stdout.into_bytes())?;
    let provider = ClaudeProvider::new(
        ClaudeProviderConfig::new().with_persona_prompt("reviewer", "review-persona")?,
    )?;
    let outcome = execute_provider(provider, &harness)?;

    assert!(outcome.is_completed());
    assert_eq!(outcome.output(), Some("done\n\n"));
    let cost = outcome
        .evidence()
        .cost()
        .ok_or_else(|| std::io::Error::other("cost evidence missing"))?;
    assert_eq!(cost.input_tokens(), 9);
    assert_eq!(cost.output_tokens(), 5);
    assert_eq!(cost.total_cost_usd(), 0.5);
    let recorded = harness
        .recorded
        .lock()
        .map_err(|_| std::io::Error::other("request record mutex poisoned"))?;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].executable_id, "claude");
    assert_eq!(recorded[0].working_root, "/fixture/worker");
    assert!(
        recorded[0]
            .arguments
            .iter()
            .any(|arg| arg == "--strict-mcp-config")
    );
    assert!(
        recorded[0]
            .environment
            .contains(&(ENTRYPOINT_KEY.to_owned(), ENTRYPOINT_VALUE.to_owned()))
    );
    Ok(())
}

#[test]
fn malformed_output_retains_no_partial_text_or_secret() -> TestResult {
    const SECRET: &str = "PROVIDER_OUTPUT_MUST_NOT_SURVIVE";
    let stdout = format!(
        "{PRODUCTION_INIT}{{\"type\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{SECRET}\"}}]}}\n{{\"type\":\"result\",\"secret\":\"{SECRET}\"\n"
    );
    let harness = Harness::new(stdout.into_bytes())?;
    let provider = ClaudeProvider::new(ClaudeProviderConfig::new())?;
    let outcome = execute_provider(provider, &harness)?;

    assert!(!outcome.is_completed());
    assert_eq!(outcome.output(), None);
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::ContractViolation)
    );
    assert!(!format!("{outcome:?}").contains(SECRET));
    for failure in outcome.failures() {
        assert!(!format!("{failure:?}").contains(SECRET));
        assert!(!failure.to_string().contains(SECRET));
        assert!(!failure.expose_detail().contains(SECRET));
    }
    Ok(())
}

#[test]
fn production_source_has_no_process_or_ambient_environment_escape_hatch() {
    for forbidden in [
        "std::process",
        "Command::",
        "std::env",
        "var_os(",
        "vars_os(",
        "Box<dyn",
        "Arc<dyn",
    ] {
        assert!(
            !PROVIDER_SOURCE.contains(forbidden),
            "production provider contains forbidden token {forbidden}"
        );
    }
    assert!(PROVIDER_SOURCE.contains("context.run_process(&process)"));
    assert!(PROVIDER_SOURCE.contains("impl PhaseExecutor for ClaudeProvider"));
}

fn execution_request() -> Result<ExecutionRequest, ContractError> {
    execution_request_with(
        "Review the implementation".to_owned(),
        "prior evidence".to_owned(),
    )
}

fn execution_request_with(
    objective: String,
    prior_context: String,
) -> Result<ExecutionRequest, ContractError> {
    execution_request_with_target(objective, prior_context, None)
}

fn execution_request_with_target(
    objective: String,
    prior_context: String,
    target_dir: Option<std::path::PathBuf>,
) -> Result<ExecutionRequest, ContractError> {
    ExecutionRequest::new(ExecutionRequestDraft {
        mission: "mission-1".to_owned(),
        phase: "phase-1".to_owned(),
        attempt: 1,
        revision: 1,
        objective,
        persona: "reviewer".to_owned(),
        role: "review".to_owned(),
        domain: "dev".to_owned(),
        skills: vec!["rust-best-practices".to_owned()],
        dependencies: Vec::new(),
        expected_evidence: Vec::new(),
        constraints: vec!["fixture-only".to_owned()],
        prior_context,
        runtime: RuntimeFamily::parse("claude")?,
        model: "claude-opus-4-8".to_owned(),
        effort: Effort::High,
        max_turns: 20,
        worker_dir: "/fixture/worker".into(),
        target_dir,
        resume_from: None,
        hook_script: None,
    })
}

fn execute_provider(provider: ClaudeProvider, harness: &Harness) -> TestResult<AttemptOutcome> {
    let mut registry = ExecutorRegistry::new();
    let _previous = registry.register("claude", Arc::new(provider))?;
    let resolved = registry.resolve("claude")?;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        harness,
        harness,
        harness,
        harness,
        &mut sink,
        WorkerIdentity::new("mission-1", "phase-1", "worker-1")?,
        harness.now + Duration::from_secs(60),
    );
    Ok(resolved.execute(&execution_request()?, &mut context)?)
}

#[derive(Clone)]
struct RecordedRequest {
    executable_id: String,
    working_root: String,
    arguments: Vec<String>,
    environment: Vec<(String, String)>,
}

struct Harness {
    now: Instant,
    receipt: ProcessReceipt,
    process_error: ProcessServiceError,
    effect_error: EffectServiceError,
    recorded: Mutex<Vec<RecordedRequest>>,
}

impl Harness {
    fn new(stdout: Vec<u8>) -> TestResult<Self> {
        Ok(Self {
            now: Instant::now(),
            receipt: ProcessReceipt::new(
                ProcessTerminationReceipt::Exited(ProcessExitStatus::code(0)?),
                stdout,
                Vec::new(),
                0,
                0,
                true,
                Duration::from_millis(10),
            )?,
            process_error: ProcessServiceError::new(
                orchestrator_exec::ProcessServiceErrorKind::InvalidRequest,
                "fixture could not record process request",
            )?,
            effect_error: EffectServiceError::new(
                EffectServiceErrorKind::Denied,
                "effects are disabled in the Claude provider fixture",
            )?,
            recorded: Mutex::new(Vec::new()),
        })
    }
}

impl Cancellation for Harness {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl Clock for Harness {
    fn now(&self) -> Instant {
        self.now
    }
}

impl WatchdogPolicy for Harness {
    fn evaluate(&self, _now: Instant, last_activity: Instant) -> WatchdogDecision {
        WatchdogDecision::Continue {
            next_check: last_activity + Duration::from_secs(30),
        }
    }

    fn stall_window(&self) -> Duration {
        Duration::from_secs(30)
    }
}

impl ProcessService for Harness {
    fn finish_preflight(
        &self,
        request: &ProcessRequest,
        preflight: orchestrator_exec::ProcessPreflight<'_>,
    ) -> Result<(), ProcessServiceError> {
        if preflight.bind(self, request).is_some() {
            Ok(())
        } else {
            Err(self.process_error.clone())
        }
    }

    fn execute(
        &self,
        request: &ProcessRequest,
        _budget: ProcessBudget,
    ) -> Result<ProcessReceipt, ProcessServiceError> {
        let recorded = RecordedRequest {
            executable_id: request.executable_id().to_owned(),
            working_root: request.working_root().to_string_lossy().into_owned(),
            arguments: request.expose_arguments().to_vec(),
            environment: request.expose_environment().to_vec(),
        };
        self.recorded
            .lock()
            .map_err(|_| self.process_error.clone())?
            .push(recorded);
        Ok(self.receipt.clone())
    }
}

impl EffectService for Harness {
    fn execute(
        &self,
        _request: &EffectRequest,
        _budget: EffectBudget<'_>,
    ) -> Result<orchestrator_exec::EffectReceipt, EffectServiceError> {
        Err(self.effect_error.clone())
    }
}

struct RecordingSink {
    next_sequence: i64,
    receipt_error: EventSinkError,
}

impl RecordingSink {
    fn new() -> Result<Self, WorkerEventError> {
        Ok(Self {
            next_sequence: 1,
            receipt_error: EventSinkError::new(
                EventSinkErrorKind::Unavailable,
                "fixture event receipt allocator is exhausted",
            )?,
        })
    }
}

impl EventSink for RecordingSink {
    fn emit(&mut self, _event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError> {
        let sequence = self.next_sequence;
        let receipt = EventReceipt::new(
            format!("evt_claude_fixture_{sequence:016x}"),
            format!("2026-07-16T00:00:00.{sequence:09}Z"),
            sequence,
        )
        .map_err(|_| self.receipt_error.clone())?;
        self.next_sequence = sequence
            .checked_add(1)
            .ok_or_else(|| self.receipt_error.clone())?;
        Ok(receipt)
    }
}

/// Sanitized from real Claude Code 2.1.269 first-use pilot records:
/// identifiers, paths, thinking, signatures and review text are replaced,
/// and repeated deltas are collapsed. The reviewed informational rate-limit
/// shape is included without granting it output authority.
const RECORDED_2_1_269_WIRE: &str =
    include_str!("../tests/fixtures/claude-2.1.269-first-use-pilot-success.jsonl");
const RECORDED_2_1_269_REVIEW: &str = "Sanitized review: no findings.";

fn parse_first_use_pilot(stdout: &str) -> Result<ParsedOutput, ClaudeOutputError> {
    parse_one_shot_output_with_contract(
        stdout.as_bytes(),
        ClaudeProtocolContract::first_use_pilot(),
    )
}

fn recorded_wire_with(from: &str, to: &str) -> TestResult<String> {
    let occurrences = RECORDED_2_1_269_WIRE.matches(from).count();
    if occurrences != 1 {
        return Err(format!("{from} occurs {occurrences} times in the recorded wire").into());
    }
    Ok(RECORDED_2_1_269_WIRE.replacen(from, to, 1))
}

fn assert_pilot_refuses(case: &str, stdout: &str, expected: ClaudeOutputError) -> TestResult {
    match parse_first_use_pilot(stdout) {
        Ok(_) => Err(format!("{case}: pilot contract accepted the stream").into()),
        Err(error) => {
            assert_eq!(error, expected, "{case}");
            Ok(())
        }
    }
}

#[test]
fn observed_rate_limit_event_is_validated_and_dropped() -> TestResult {
    let record = RECORDED_2_1_269_WIRE
        .lines()
        .find(|line| line.contains(r#""type":"rate_limit_event""#))
        .ok_or("rate-limit record")?;
    match first_use_pilot_wire::normalize(record)? {
        first_use_pilot_wire::PilotLine::Drop => Ok(()),
        first_use_pilot_wire::PilotLine::Strict(_) => {
            Err("informational rate-limit record reached the strict parser".into())
        }
    }
}

#[test]
fn recorded_2_1_269_success_completes_only_under_the_pilot_contract() -> TestResult {
    let parsed = parse_first_use_pilot(RECORDED_2_1_269_WIRE)?;
    assert_eq!(parsed.output, format!("{RECORDED_2_1_269_REVIEW}\n\n"));
    let cost = parsed.cost.ok_or("recorded result carries cost")?;
    assert_eq!(cost.total_cost_usd(), 0.0453148);
    assert_eq!(cost.output_tokens(), 2325);
    assert_eq!(cost.input_tokens(), 2 + 4568 + 3289);

    // The observed first-use failure: the production contract rejects the
    // same bytes, and still does so when only the version is pinned back.
    assert!(matches!(
        parse_one_shot_output(RECORDED_2_1_269_WIRE.as_bytes()),
        Err(ClaudeOutputError::Malformed)
    ));
    let version_only = recorded_wire_with(
        r#""claude_code_version":"2.1.269""#,
        r#""claude_code_version":"2.1.211""#,
    )?;
    assert!(matches!(
        parse_one_shot_output(version_only.as_bytes()),
        Err(ClaudeOutputError::Malformed)
    ));
    Ok(())
}

#[test]
fn production_contract_still_rejects_each_2_1_269_addition() -> TestResult {
    let assistant = "{\"type\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"x\"}]}\n";
    let result = "{\"type\":\"result\",\"subtype\":\"success\"}\n";
    let init_with_socket = PRODUCTION_INIT.replace(
        r#""fast_mode_state":"off""#,
        r#""fast_mode_state":"off","messaging_socket_path":"/s""#,
    );
    let init_with_agents = PRODUCTION_INIT.replace(r#""agents":[]"#, r#""agents":["Explore"]"#);
    for (case, stdout, expected) in [
        (
            "rate_limit_event",
            format!(
                "{PRODUCTION_INIT}{{\"type\":\"rate_limit_event\",\"rate_limit_info\":{{\"status\":\"allowed_warning\"}}}}\n{assistant}{result}"
            ),
            ClaudeOutputError::UnknownProtocolValue,
        ),
        (
            "system/status",
            format!(
                "{PRODUCTION_INIT}{{\"type\":\"system\",\"subtype\":\"status\",\"status\":\"requesting\"}}\n{assistant}{result}"
            ),
            ClaudeOutputError::UnknownProtocolValue,
        ),
        (
            "init socket",
            format!("{init_with_socket}{assistant}{result}"),
            ClaudeOutputError::Malformed,
        ),
        (
            "init agents",
            format!("{init_with_agents}{assistant}{result}"),
            ClaudeOutputError::UnisolatedRuntime,
        ),
        (
            "assistant timestamp",
            format!(
                "{PRODUCTION_INIT}{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"x\"}}]}},\"timestamp\":\"t\"}}\n{result}"
            ),
            ClaudeOutputError::Malformed,
        ),
        (
            "result subagent_stats",
            format!(
                "{PRODUCTION_INIT}{assistant}{{\"type\":\"result\",\"subtype\":\"success\",\"subagent_stats\":{{}}}}\n"
            ),
            ClaudeOutputError::Malformed,
        ),
    ] {
        match parse_one_shot_output(stdout.as_bytes()) {
            Ok(_) => {
                return Err(format!("{case}: production contract accepted 2.1.269 drift").into());
            }
            Err(error) => assert_eq!(error, expected, "{case}"),
        }
    }
    Ok(())
}

#[test]
fn pilot_contract_refuses_errors_malformed_and_forbidden_activity() -> TestResult {
    let review_block = format!(r#"[{{"type":"text","text":"{RECORDED_2_1_269_REVIEW}"}}]"#);
    let init_end = RECORDED_2_1_269_WIRE.find('\n').ok_or("init line")? + 1;
    let (init, rest) = RECORDED_2_1_269_WIRE.split_at(init_end);
    let hook = r#"{"type":"system","subtype":"hook_started","hook_id":"h","hook_name":"SessionStart:startup","hook_event":"SessionStart"}"#;
    let without_result = &RECORDED_2_1_269_WIRE[..RECORDED_2_1_269_WIRE
        .trim_end()
        .rfind('\n')
        .ok_or("result line")?
        + 1];

    let cases = [
        (
            "result error",
            recorded_wire_with(r#""subtype":"success""#, r#""subtype":"error""#)?,
            ClaudeOutputError::ProviderReportedError,
        ),
        (
            "result is_error",
            recorded_wire_with(r#""is_error":false"#, r#""is_error":true"#)?,
            ClaudeOutputError::ProviderReportedError,
        ),
        (
            "rate limit rejected",
            recorded_wire_with(r#""status":"allowed""#, r#""status":"rejected""#)?,
            ClaudeOutputError::ProviderReportedError,
        ),
        (
            "rate limit unknown primary status",
            recorded_wire_with(r#""status":"allowed""#, r#""status":"deferred""#)?,
            ClaudeOutputError::UnknownProtocolValue,
        ),
        (
            "rate limit unknown overage status",
            recorded_wire_with(
                r#""overageStatus":"rejected""#,
                r#""overageStatus":"allowed""#,
            )?,
            ClaudeOutputError::UnknownProtocolValue,
        ),
        (
            "rate limit unknown overage reason",
            recorded_wire_with(
                r#""overageDisabledReason":"out_of_credits""#,
                r#""overageDisabledReason":"billing_error""#,
            )?,
            ClaudeOutputError::UnknownProtocolValue,
        ),
        (
            "rate limit non-string overage status",
            recorded_wire_with(r#""overageStatus":"rejected""#, r#""overageStatus":false"#)?,
            ClaudeOutputError::Malformed,
        ),
        (
            "rate limit non-string overage reason",
            recorded_wire_with(
                r#""overageDisabledReason":"out_of_credits""#,
                r#""overageDisabledReason":false"#,
            )?,
            ClaudeOutputError::Malformed,
        ),
        (
            "rate limit unknown field",
            recorded_wire_with(
                r#""overageDisabledReason":"out_of_credits""#,
                r#""overageDisabledReason":"out_of_credits","unreviewed":false"#,
            )?,
            ClaudeOutputError::Malformed,
        ),
        (
            "rate limit duplicate overage status",
            recorded_wire_with(
                r#""overageStatus":"rejected""#,
                r#""overageStatus":"rejected","overageStatus":"rejected""#,
            )?,
            ClaudeOutputError::Malformed,
        ),
        (
            "missing result",
            without_result.to_owned(),
            ClaudeOutputError::MissingResult,
        ),
        (
            "truncated json",
            recorded_wire_with(r#""status":"requesting","#, r#""status":"requesting""#)?,
            ClaudeOutputError::Malformed,
        ),
        (
            "duplicate drift key",
            recorded_wire_with(r#""ttft_ms":1656"#, r#""ttft_ms":1656,"ttft_ms":1"#)?,
            ClaudeOutputError::Malformed,
        ),
        (
            "unknown init field",
            recorded_wire_with(
                r#""messaging_socket_path":"#,
                r#""unreviewed":1,"messaging_socket_path":"#,
            )?,
            ClaudeOutputError::Malformed,
        ),
        (
            "unknown result field",
            recorded_wire_with(r#""result_index":0"#, r#""result_index":0,"unreviewed":0"#)?,
            ClaudeOutputError::Malformed,
        ),
        (
            "non-null container",
            recorded_wire_with(
                r#""content":[],"container":null"#,
                r#""content":[],"container":{}"#,
            )?,
            ClaudeOutputError::UnknownProtocolValue,
        ),
        (
            "unknown status",
            recorded_wire_with(r#""status":"requesting""#, r#""status":"compacting""#)?,
            ClaudeOutputError::UnknownProtocolValue,
        ),
        (
            "tool use",
            recorded_wire_with(
                &review_block,
                r#"[{"type":"tool_use","id":"t","name":"Bash","input":{}}]"#,
            )?,
            ClaudeOutputError::ToolProtocol,
        ),
        (
            "input json delta",
            recorded_wire_with(
                r#"{"type":"signature_delta","signature":"c2FuaXRpemVk"}"#,
                r#"{"type":"input_json_delta","partial_json":"{}"}"#,
            )?,
            ClaudeOutputError::ToolProtocol,
        ),
        (
            "subagent spawned",
            recorded_wire_with(r#""spawned":0"#, r#""spawned":1"#)?,
            ClaudeOutputError::ToolProtocol,
        ),
        (
            "web search",
            recorded_wire_with(r#""web_search_requests":0"#, r#""web_search_requests":1"#)?,
            ClaudeOutputError::ToolProtocol,
        ),
        (
            "hook activity",
            format!("{init}{hook}\n{rest}"),
            ClaudeOutputError::UnisolatedRuntime,
        ),
        (
            "mcp server",
            recorded_wire_with(
                r#""mcp_servers":[]"#,
                r#""mcp_servers":[{"name":"x","status":"connected"}]"#,
            )?,
            ClaudeOutputError::UnisolatedRuntime,
        ),
        (
            "tools",
            recorded_wire_with(r#""tools":[]"#, r#""tools":["Bash"]"#)?,
            ClaudeOutputError::UnisolatedRuntime,
        ),
        (
            "custom agent",
            recorded_wire_with(r#""agents":["claude","#, r#""agents":["reviewer","#)?,
            ClaudeOutputError::UnisolatedRuntime,
        ),
        (
            "unobserved capability",
            recorded_wire_with(
                r#""capabilities":["#,
                r#""capabilities":["remote_control_v1","#,
            )?,
            ClaudeOutputError::UnisolatedRuntime,
        ),
        (
            "record before init",
            format!(
                "{}{init}{}",
                rest.lines()
                    .nth(1)
                    .map(|line| format!("{line}\n"))
                    .ok_or("rate limit line")?,
                rest
            ),
            ClaudeOutputError::UnisolatedRuntime,
        ),
        (
            "wrong version",
            recorded_wire_with(
                r#""claude_code_version":"2.1.269""#,
                r#""claude_code_version":"2.1.270""#,
            )?,
            ClaudeOutputError::UnknownProtocolValue,
        ),
        (
            "auxiliary provider",
            recorded_wire_with(
                r#""provider":"firstParty","costBasis":"list"},"claude-sonnet-5""#,
                r#""provider":"bedrock","costBasis":"list"},"claude-sonnet-5""#,
            )?,
            ClaudeOutputError::UnknownProtocolValue,
        ),
        (
            "non-message iteration",
            recorded_wire_with(
                r#""ephemeral_1h_input_tokens":4568},"type":"message"}]},"context_management""#,
                r#""ephemeral_1h_input_tokens":4568},"type":"tool"}]},"context_management""#,
            )?,
            ClaudeOutputError::UnknownProtocolValue,
        ),
        (
            "cost mismatch",
            recorded_wire_with(r#""total_cost_usd":0.0453148"#, r#""total_cost_usd":0.05"#)?,
            ClaudeOutputError::InvalidCost,
        ),
    ];
    for (case, stdout, expected) in cases {
        assert_pilot_refuses(case, &stdout, expected)?;
    }
    Ok(())
}

const RECORDED_2_1_269_CODING_SUCCESS: &str =
    include_str!("../tests/fixtures/claude-2.1.269-coding-success.jsonl");
const RECORDED_2_1_269_CODING_DENIED: &str =
    include_str!("../tests/fixtures/claude-2.1.269-coding-denied.jsonl");

fn parse_coding(stdout: &str) -> Result<ParsedOutput, ClaudeOutputError> {
    coding::parse_coding_output(stdout.as_bytes(), Path::new("/workspace"), "sonnet", 5)
}

#[test]
fn coding_contract_accepts_sanitized_file_tool_success_with_cost() -> TestResult {
    let parsed = parse_coding(RECORDED_2_1_269_CODING_SUCCESS)?;

    assert_eq!(
        parsed.output,
        "Changed example.txt from before to after.\n\n"
    );
    assert_eq!(parsed.cost.ok_or("missing cost")?.total_cost_usd(), 0.01);
    Ok(())
}

#[test]
fn coding_contract_rejects_denial_even_when_final_result_says_success() {
    assert!(matches!(
        parse_coding(RECORDED_2_1_269_CODING_DENIED),
        Err(ClaudeOutputError::PermissionDenied)
    ));
}

#[test]
fn coding_contract_rejects_outside_path_and_unknown_tool_family() {
    let outside = RECORDED_2_1_269_CODING_SUCCESS.replacen(
        r#""file_path":"/workspace/example.txt""#,
        r#""file_path":"/outside/sentinel.txt""#,
        1,
    );
    let unknown =
        RECORDED_2_1_269_CODING_SUCCESS.replacen(r#""name":"Read""#, r#""name":"Bash""#, 1);

    assert!(matches!(
        parse_coding(&outside),
        Err(ClaudeOutputError::ToolPath)
    ));
    assert!(matches!(
        parse_coding(&unknown),
        Err(ClaudeOutputError::ForbiddenCodingActivity)
    ));
}

#[test]
fn coding_contract_rejects_init_turn_protected_write_and_cost_drift() {
    let tools = RECORDED_2_1_269_CODING_SUCCESS.replacen(
        r#""tools":["Edit","Glob","Grep","Read","Write"]"#,
        r#""tools":["Bash","Edit","Glob","Grep","Read","Write"]"#,
        1,
    );
    let model = RECORDED_2_1_269_CODING_SUCCESS.replacen(
        r#""model":"claude-sonnet-5""#,
        r#""model":"claude-opus-5""#,
        1,
    );
    let turns = RECORDED_2_1_269_CODING_SUCCESS.replacen(r#""num_turns":3"#, r#""num_turns":6"#, 1);
    let protected = RECORDED_2_1_269_CODING_SUCCESS
        .replace("/workspace/example.txt", "/workspace/.claude/settings.json");
    let no_cost = RECORDED_2_1_269_CODING_SUCCESS.replacen(r#","total_cost_usd":0.01"#, "", 1);

    assert!(matches!(
        parse_coding(&tools),
        Err(ClaudeOutputError::UnisolatedRuntime)
    ));
    assert!(matches!(
        parse_coding(&model),
        Err(ClaudeOutputError::UnisolatedRuntime)
    ));
    assert!(matches!(
        parse_coding(&turns),
        Err(ClaudeOutputError::ProviderReportedError)
    ));
    assert!(matches!(
        parse_coding(&protected),
        Err(ClaudeOutputError::ToolPath)
    ));
    assert!(parse_coding(&no_cost).is_err());
}

#[test]
fn coding_contract_rejects_missing_duplicate_truncated_and_tool_only_results() -> TestResult {
    let lines = RECORDED_2_1_269_CODING_SUCCESS.lines().collect::<Vec<_>>();
    let missing_result = lines[..lines.len().saturating_sub(1)].join("\n");
    let duplicate_result = format!(
        "{}\n{}\n",
        RECORDED_2_1_269_CODING_SUCCESS.trim_end(),
        lines.last().ok_or("result line")?
    );
    let truncated = format!("{}{{\"type\":", RECORDED_2_1_269_CODING_SUCCESS);
    let tool_only = RECORDED_2_1_269_CODING_SUCCESS.replace(
        r#"{"type":"text","text":"Changed example.txt from before to after."}"#,
        r#"{"type":"text","text":""}"#,
    );

    assert!(matches!(
        parse_coding(&missing_result),
        Err(ClaudeOutputError::MissingResult)
    ));
    assert!(matches!(
        parse_coding(&duplicate_result),
        Err(ClaudeOutputError::Malformed)
    ));
    assert!(matches!(
        parse_coding(&truncated),
        Err(ClaudeOutputError::Malformed)
    ));
    assert!(matches!(
        parse_coding(&tool_only),
        Err(ClaudeOutputError::EmptyOutput)
    ));
    Ok(())
}
