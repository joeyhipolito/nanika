use serde::ser::{SerializeMap, Serializer};
use serde_json::{Map, Number, Value};
use std::{collections::BTreeMap, fmt, ops::Range};
use thiserror::Error;

const CHECKPOINT_ENVELOPE_VERSION: u64 = 1;
const CHECKPOINT_PAYLOAD_VERSION: u64 = 2;
/// Maximum JSON-object content accepted by Go's one-MiB `bufio.Scanner`
/// configuration once the required LF delimiter occupies one token byte.
///
/// Go's scanner rejects a full one-MiB buffer when it has not yet observed a
/// delimiter, so the largest interoperable JSON record is one byte smaller
/// than the scanner token ceiling. The same bound also applies to a final EOF
/// token because `os.File.Read` reports EOF on the following read.
pub const GO_EVENT_JSON_CONTENT_MAX_BYTES: usize = (1024 * 1024) - 1;
pub const GO_ZERO_TIME: &str = "0001-01-01T00:00:00Z";

/// The checkpoint shape found on disk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointSourceShape {
    LegacyDirect,
    EnvelopeV1,
}

/// A decoded checkpoint together with its preservation source.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedCheckpoint {
    pub source_shape: CheckpointSourceShape,
    pub projection: CheckpointProjection,
    original: Value,
}

/// Typed checkpoint payload used by the bounded current writer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CheckpointProjection {
    pub workspace_id: String,
    pub domain: String,
    pub plan: Option<CheckpointPlan>,
    pub status: String,
    pub started_at: String,
    pub git_repo_root: String,
    pub worktree_path: String,
    pub branch_name: String,
    pub base_branch: String,
    pub extra: BTreeMap<String, Value>,
    pub envelope_extra: BTreeMap<String, Value>,
}

/// Typed plan fields plus forward-compatible fields not understood by this slice.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CheckpointPlan {
    pub id: String,
    pub task: String,
    pub phases: Vec<CheckpointPhase>,
    pub execution_mode: String,
    pub decomp_source: String,
    pub created_at: String,
    pub extra: BTreeMap<String, Value>,
}

/// Typed phase identity/state plus all forward-compatible phase fields.
///
/// Field order mirrors Go's `core.Phase` declaration exactly
/// (`internal/core/types.go:33-130` in the frozen Go snapshot) so
/// [`CurrentPhase`]'s writer emits reserved keys in the same order Go's
/// `encoding/json` would. Fields tagged `omitempty` in Go are typed here as
/// their bare value (not `Option<T>`) and are skipped at serialize time
/// when they hold Go's zero value (empty string/slice, `0`, `0.0`,
/// `false`) via `serialize_non_empty`/`serialize_non_zero_*`/`serialize_true`
/// helpers — this is behaviorally identical to Go's `omitempty` for every
/// type used here. Fields with no `omitempty` (`id` through `expected` and
/// `status`) are always emitted, matching Go's plain zero-value encoding.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CheckpointPhase {
    pub id: String,
    pub name: String,
    pub objective: String,
    pub persona: String,
    pub model_tier: String,
    pub skills: Vec<String>,
    pub constraints: Vec<String>,
    pub dependencies: Vec<String>,
    pub expected: String,
    /// `Role` (`internal/core/role.go:15`); `json:"role,omitempty"`
    /// (`types.go:46`).
    pub role: String,
    /// `json:"target_dir,omitempty"` (`types.go:52`).
    pub target_dir: String,
    /// `json:"priority,omitempty"` (`types.go:56`).
    pub priority: String,
    /// `Runtime` (`internal/core/runtime.go:15`); `json:"runtime,omitempty"`
    /// (`types.go:60`).
    pub runtime: String,
    /// `json:"runtime_policy_applied,omitempty"` (`types.go:63`).
    pub runtime_policy_applied: bool,
    /// `time.Duration` nanoseconds; `json:"stall_timeout,omitempty"`
    /// (`types.go:68`).
    pub stall_timeout: i64,
    pub status: String,
    /// `json:"output,omitempty"` (`types.go:72`).
    pub output: String,
    /// `json:"error,omitempty"` (`types.go:73`).
    pub error: String,
    /// `*time.Time`; `json:"start_time,omitempty"` (`types.go:74`). Go's nil
    /// pointer omits the key entirely (never a zero-time substitute), so
    /// this is skipped when empty rather than defaulted to
    /// [`GO_ZERO_TIME`].
    pub start_time: String,
    /// `*time.Time`; `json:"end_time,omitempty"` (`types.go:75`). Same
    /// omit-when-absent semantics as `start_time`.
    pub end_time: String,
    /// `json:"signal_remainder,omitempty"` (`types.go:78`).
    pub signal_remainder: String,
    /// `json:"retries,omitempty"` (`types.go:81`).
    pub retries: i64,
    /// `json:"gate_passed,omitempty"` (`types.go:82`).
    pub gate_passed: bool,
    /// `json:"output_len,omitempty"` (`types.go:83`).
    pub output_len: i64,
    /// `json:"parsed_skills,omitempty"` (`types.go:84`).
    pub parsed_skills: Vec<String>,
    /// `json:"learnings_retrieved,omitempty"` (`types.go:85`).
    pub learnings_retrieved: i64,
    /// `json:"session_id,omitempty"` (`types.go:86`).
    pub session_id: String,
    /// `json:"persona_selection_method,omitempty"` (`types.go:87`).
    pub persona_selection_method: String,
    /// `json:"model,omitempty"` (`types.go:89`).
    pub model: String,
    /// `json:"tokens_in,omitempty"` (`types.go:90`).
    pub tokens_in: i64,
    /// `json:"tokens_out,omitempty"` (`types.go:91`).
    pub tokens_out: i64,
    /// `json:"tokens_cache_creation,omitempty"` (`types.go:92`).
    pub tokens_cache_creation: i64,
    /// `json:"tokens_cache_read,omitempty"` (`types.go:93`).
    pub tokens_cache_read: i64,
    /// `json:"cost_usd,omitempty"` (`types.go:94`).
    pub cost_usd: f64,
    /// `json:"review_iteration,omitempty"` (`types.go:97`).
    pub review_iteration: i64,
    /// `json:"origin_phase_id,omitempty"` (`types.go:98`).
    pub origin_phase_id: String,
    /// `json:"max_review_loops,omitempty"` (`types.go:99`).
    pub max_review_loops: i64,
    /// `json:"parse_retry_count,omitempty"` (`types.go:100`).
    pub parse_retry_count: i64,
    /// `json:"review_blockers,omitempty"` (`types.go:101`).
    pub review_blockers: Vec<String>,
    /// `json:"review_warnings,omitempty"` (`types.go:102`).
    pub review_warnings: Vec<String>,
    /// `json:"changed_files,omitempty"` (`types.go:107`).
    pub changed_files: Vec<String>,
    /// `json:"output_bytes,omitempty"` (`types.go:113`).
    pub output_bytes: i64,
    /// `json:"worker,omitempty"` (`types.go:119`).
    pub worker: String,
    /// `json:"barok_applied,omitempty"` (`types.go:125`).
    pub barok_applied: i64,
    /// `json:"barok_retry,omitempty"` (`types.go:126`).
    pub barok_retry: i64,
    /// `json:"barok_validator_ms,omitempty"` (`types.go:127`).
    pub barok_validator_ms: i64,
    /// `json:"barok_retry_validator_ms,omitempty"` (`types.go:128`).
    pub barok_retry_validator_ms: i64,
    /// `json:"barok_first_run_session_id,omitempty"` (`types.go:129`).
    pub barok_first_run_session_id: String,
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Error)]
pub enum CheckpointError {
    #[error("checkpoint is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("checkpoint root must be an object")]
    RootNotObject,
    #[error("checkpoint matched neither legacy direct nor envelope-v1 shape: {0}")]
    UnsupportedShape(String),
    #[error("checkpoint field {field} has invalid type; expected {expected}")]
    InvalidField {
        field: String,
        expected: &'static str,
    },
}

/// Decodes legacy direct or envelope-v1 checkpoints without ambient I/O.
pub fn decode_checkpoint(bytes: &[u8]) -> Result<DecodedCheckpoint, CheckpointError> {
    let original: Value = serde_json::from_slice(bytes)?;
    let root = original.as_object().ok_or(CheckpointError::RootNotObject)?;

    let legacy_error = if non_empty_string(root.get("workspace_id")) {
        match parse_checkpoint_payload(root.clone(), BTreeMap::new()) {
            Ok(projection) => {
                return Ok(DecodedCheckpoint {
                    source_shape: CheckpointSourceShape::LegacyDirect,
                    projection,
                    original,
                });
            }
            Err(error) => Some(error.to_string()),
        }
    } else {
        Some("top-level workspace_id is empty".to_owned())
    };

    let version = root.get("version").and_then(Value::as_u64);
    if version != Some(CHECKPOINT_ENVELOPE_VERSION) {
        return Err(CheckpointError::UnsupportedShape(format!(
            "legacy attempt: {}; envelope version is {:?}",
            legacy_error.as_deref().unwrap_or("unknown failure"),
            version
        )));
    }
    let payload = root
        .get("payload")
        .and_then(Value::as_object)
        .ok_or_else(|| CheckpointError::InvalidField {
            field: "payload".to_owned(),
            expected: "object",
        })?;
    if !non_empty_string(payload.get("workspace_id")) {
        return Err(CheckpointError::UnsupportedShape(
            "payload.workspace_id is empty".to_owned(),
        ));
    }
    let mut envelope_extra = object_to_btree(root);
    envelope_extra.remove("version");
    envelope_extra.remove("payload");
    let projection = parse_checkpoint_payload(payload.clone(), envelope_extra)?;
    Ok(DecodedCheckpoint {
        source_shape: CheckpointSourceShape::EnvelopeV1,
        projection,
        original,
    })
}

/// Encodes only envelope-v1/payload-v2 while carrying unknown values forward.
pub fn encode_current_checkpoint(value: &CheckpointProjection) -> Result<Vec<u8>, CheckpointError> {
    if value.workspace_id.is_empty() {
        return Err(CheckpointError::UnsupportedShape(
            "current checkpoint workspace_id is empty".to_owned(),
        ));
    }
    if !value.started_at.is_empty() && !is_rfc3339(&value.started_at) {
        return Err(CheckpointError::InvalidField {
            field: "started_at".to_owned(),
            expected: "RFC3339 string",
        });
    }
    if let Some(plan) = &value.plan {
        validate_current_plan_fields(plan)?;
    }
    let json = serde_json::to_string_pretty(&CurrentCheckpoint(value))?;
    Ok(escape_go_json_for_html(&json).into_bytes())
}

/// Encodes a plan for `plan.json`, reusing the checkpoint payload's plan
/// serializer (`CurrentPlan`) so `plan.json` and checkpoint-plan vocabulary
/// cannot drift.
///
/// Matches Go's exact `plan.json` byte format: two-space
/// `json.MarshalIndent(plan, "", "  ")` with no trailing newline, written by
/// `os.WriteFile(ws.Path+"/plan.json", planData, 0600)`
/// (`skills/orchestrator/internal/cmd/run.go:444-445` in the frozen Go
/// snapshot), with Go's unconditional HTML escaping of `<`, `>`, `&`,
/// U+2028, and U+2029 (`encoding/json`'s `Marshal`/`MarshalIndent` default)
/// applied as a post-processing step via `escape_go_json_for_html`.
///
/// Go's `core.Plan`/`core.Phase` (`internal/core/types.go:9-16, 33-130`)
/// have every declared field mirrored here as a typed [`CheckpointPlan`]/
/// [`CheckpointPhase`] field, in Go's declaration order, with the same
/// `omitempty` semantics Go applies (its zero value omitted; no
/// `omitempty` always emitted). A plan/phase whose fields are all
/// modeled — the normal case for every real decomposed mission plan, since
/// this module now tracks the complete Go field set rather than a subset —
/// serializes byte-identically to a freshly Go-written `plan.json`.
///
/// Two divergences remain, deliberately:
/// - Unknown/forward-compatible values carried in a plan's or phase's
///   `extra` map are emitted *before* that object's reserved fields. This
///   is a Rust-writer convention with no Go counterpart, since every field
///   Go's writer can ever produce is now modeled directly.
/// - `Plan.Phases` has no `omitempty` (`types.go:12`), so Go's nil slice
///   marshals to `null` while this writer always emits `phases` as an
///   array (`[]` when empty). This only surfaces for a plan with zero
///   phases, which a real decomposed mission plan never has.
pub fn encode_current_plan(value: &CheckpointPlan) -> Result<Vec<u8>, CheckpointError> {
    validate_current_plan_fields(value)?;
    let json = serde_json::to_string_pretty(&CurrentPlan(value))?;
    Ok(escape_go_json_for_html(&json).into_bytes())
}

fn validate_current_plan_fields(plan: &CheckpointPlan) -> Result<(), CheckpointError> {
    if !plan.created_at.is_empty() && !is_rfc3339(&plan.created_at) {
        return Err(CheckpointError::InvalidField {
            field: "plan.created_at".to_owned(),
            expected: "RFC3339 string",
        });
    }
    for phase in &plan.phases {
        for (field, value) in [
            ("start_time", &phase.start_time),
            ("end_time", &phase.end_time),
        ] {
            if !value.is_empty() && !is_rfc3339(value) {
                return Err(CheckpointError::InvalidField {
                    field: format!("plan.phases[].{field}"),
                    expected: "RFC3339 string",
                });
            }
        }
        let extra = phase
            .extra
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        validate_phase_fields(&extra)?;
    }
    Ok(())
}

/// Re-emits the decoded source shape with all original JSON values preserved.
pub fn encode_preserved_checkpoint(value: &DecodedCheckpoint) -> Result<Vec<u8>, CheckpointError> {
    Ok(serde_json::to_vec(&value.original)?)
}

fn parse_checkpoint_payload(
    mut object: Map<String, Value>,
    envelope_extra: BTreeMap<String, Value>,
) -> Result<CheckpointProjection, CheckpointError> {
    take_optional_integer(&mut object, "version")?;
    let workspace_id = take_string(&mut object, "workspace_id")?;
    let domain = take_string(&mut object, "domain")?;
    let status = take_string(&mut object, "status")?;
    let started_at = take_optional_time(&mut object, "started_at", "started_at")?;
    let git_repo_root = take_string(&mut object, "git_repo_root")?;
    let worktree_path = take_string(&mut object, "worktree_path")?;
    let branch_name = take_string(&mut object, "branch_name")?;
    let base_branch = take_string(&mut object, "base_branch")?;
    // Sidecar-enriched fields are readable but never inserted by the current writer.
    object.remove("linear_issue_id");
    object.remove("mission_path");
    let plan = match object.remove("plan") {
        None | Some(Value::Null) => None,
        Some(Value::Object(plan)) => Some(parse_plan(plan)?),
        Some(_) => {
            return Err(CheckpointError::InvalidField {
                field: "plan".to_owned(),
                expected: "object or null",
            });
        }
    };
    Ok(CheckpointProjection {
        workspace_id,
        domain,
        plan,
        status,
        started_at,
        git_repo_root,
        worktree_path,
        branch_name,
        base_branch,
        extra: object.into_iter().collect(),
        envelope_extra,
    })
}

fn parse_plan(mut object: Map<String, Value>) -> Result<CheckpointPlan, CheckpointError> {
    let id = take_string(&mut object, "id")?;
    let task = take_string(&mut object, "task")?;
    let execution_mode = take_string(&mut object, "execution_mode")?;
    let decomp_source = take_string(&mut object, "decomp_source")?;
    let created_at = take_optional_time(&mut object, "created_at", "plan.created_at")?;
    let phases = match object.remove("phases") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(values)) => values
            .into_iter()
            .enumerate()
            .map(|(index, value)| match value {
                Value::Object(object) => parse_phase(object),
                _ => Err(CheckpointError::InvalidField {
                    field: format!("plan.phases[{index}]"),
                    expected: "object",
                }),
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(CheckpointError::InvalidField {
                field: "plan.phases".to_owned(),
                expected: "array",
            });
        }
    };
    Ok(CheckpointPlan {
        id,
        task,
        phases,
        execution_mode,
        decomp_source,
        created_at,
        extra: object.into_iter().collect(),
    })
}

fn parse_phase(mut object: Map<String, Value>) -> Result<CheckpointPhase, CheckpointError> {
    validate_phase_fields(&object)?;
    Ok(CheckpointPhase {
        id: take_string(&mut object, "id")?,
        name: take_string(&mut object, "name")?,
        objective: take_string(&mut object, "objective")?,
        persona: take_string(&mut object, "persona")?,
        model_tier: take_string(&mut object, "model_tier")?,
        skills: take_strings(&mut object, "skills")?,
        constraints: take_strings(&mut object, "constraints")?,
        dependencies: take_strings(&mut object, "dependencies")?,
        expected: take_string(&mut object, "expected")?,
        role: take_string(&mut object, "role")?,
        target_dir: take_string(&mut object, "target_dir")?,
        priority: take_string(&mut object, "priority")?,
        runtime: take_string(&mut object, "runtime")?,
        runtime_policy_applied: take_bool(&mut object, "runtime_policy_applied")?,
        stall_timeout: take_i64(&mut object, "stall_timeout")?,
        status: take_string(&mut object, "status")?,
        output: take_string(&mut object, "output")?,
        error: take_string(&mut object, "error")?,
        start_time: take_optional_time(&mut object, "start_time", "start_time")?,
        end_time: take_optional_time(&mut object, "end_time", "end_time")?,
        signal_remainder: take_string(&mut object, "signal_remainder")?,
        retries: take_i64(&mut object, "retries")?,
        gate_passed: take_bool(&mut object, "gate_passed")?,
        output_len: take_i64(&mut object, "output_len")?,
        parsed_skills: take_strings(&mut object, "parsed_skills")?,
        learnings_retrieved: take_i64(&mut object, "learnings_retrieved")?,
        session_id: take_string(&mut object, "session_id")?,
        persona_selection_method: take_string(&mut object, "persona_selection_method")?,
        model: take_string(&mut object, "model")?,
        tokens_in: take_i64(&mut object, "tokens_in")?,
        tokens_out: take_i64(&mut object, "tokens_out")?,
        tokens_cache_creation: take_i64(&mut object, "tokens_cache_creation")?,
        tokens_cache_read: take_i64(&mut object, "tokens_cache_read")?,
        cost_usd: take_f64(&mut object, "cost_usd")?,
        review_iteration: take_i64(&mut object, "review_iteration")?,
        origin_phase_id: take_string(&mut object, "origin_phase_id")?,
        max_review_loops: take_i64(&mut object, "max_review_loops")?,
        parse_retry_count: take_i64(&mut object, "parse_retry_count")?,
        review_blockers: take_strings(&mut object, "review_blockers")?,
        review_warnings: take_strings(&mut object, "review_warnings")?,
        changed_files: take_strings(&mut object, "changed_files")?,
        output_bytes: take_i64(&mut object, "output_bytes")?,
        worker: take_string(&mut object, "worker")?,
        barok_applied: take_i64(&mut object, "barok_applied")?,
        barok_retry: take_i64(&mut object, "barok_retry")?,
        barok_validator_ms: take_i64(&mut object, "barok_validator_ms")?,
        barok_retry_validator_ms: take_i64(&mut object, "barok_retry_validator_ms")?,
        barok_first_run_session_id: take_string(&mut object, "barok_first_run_session_id")?,
        extra: object.into_iter().collect(),
    })
}

fn validate_phase_fields(object: &Map<String, Value>) -> Result<(), CheckpointError> {
    const STRINGS: &[&str] = &[
        "role",
        "target_dir",
        "priority",
        "runtime",
        "output",
        "error",
        "signal_remainder",
        "session_id",
        "persona_selection_method",
        "model",
        "origin_phase_id",
        "worker",
        "barok_first_run_session_id",
    ];
    const BOOLEANS: &[&str] = &["runtime_policy_applied", "gate_passed"];
    const INTEGERS: &[&str] = &[
        "stall_timeout",
        "retries",
        "output_len",
        "learnings_retrieved",
        "tokens_in",
        "tokens_out",
        "tokens_cache_creation",
        "tokens_cache_read",
        "review_iteration",
        "max_review_loops",
        "parse_retry_count",
        "output_bytes",
        "barok_applied",
        "barok_retry",
        "barok_validator_ms",
        "barok_retry_validator_ms",
    ];
    const STRING_ARRAYS: &[&str] = &[
        "parsed_skills",
        "review_blockers",
        "review_warnings",
        "changed_files",
    ];
    for field in STRINGS {
        validate_optional(object, field, "string", Value::is_string)?;
    }
    for field in BOOLEANS {
        validate_optional(object, field, "boolean", Value::is_boolean)?;
    }
    for field in INTEGERS {
        validate_optional(object, field, "signed integer", |value| {
            value.as_i64().is_some()
        })?;
    }
    for field in STRING_ARRAYS {
        validate_optional(object, field, "array of strings", |value| {
            value
                .as_array()
                .is_some_and(|values| values.iter().all(Value::is_string))
        })?;
    }
    validate_optional(object, "cost_usd", "number", Value::is_number)?;
    for field in ["start_time", "end_time"] {
        validate_optional(object, field, "RFC3339 string", |value| {
            value.as_str().is_some_and(is_rfc3339)
        })?;
    }
    Ok(())
}

fn validate_optional(
    object: &Map<String, Value>,
    field: &str,
    expected: &'static str,
    predicate: impl FnOnce(&Value) -> bool,
) -> Result<(), CheckpointError> {
    if let Some(value) = object.get(field) {
        if !value.is_null() && !predicate(value) {
            return Err(CheckpointError::InvalidField {
                field: format!("plan.phases[].{field}"),
                expected,
            });
        }
    }
    Ok(())
}

fn non_empty_string(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
}

fn take_string(object: &mut Map<String, Value>, name: &str) -> Result<String, CheckpointError> {
    match object.remove(name) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err(CheckpointError::InvalidField {
            field: name.to_owned(),
            expected: "string",
        }),
    }
}

fn take_strings(
    object: &mut Map<String, Value>,
    name: &str,
) -> Result<Vec<String>, CheckpointError> {
    match object.remove(name) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .into_iter()
            .map(|value| match value {
                Value::String(value) => Ok(value),
                _ => Err(CheckpointError::InvalidField {
                    field: name.to_owned(),
                    expected: "array of strings",
                }),
            })
            .collect(),
        Some(_) => Err(CheckpointError::InvalidField {
            field: name.to_owned(),
            expected: "array of strings",
        }),
    }
}

fn take_optional_time(
    object: &mut Map<String, Value>,
    name: &str,
    field: &str,
) -> Result<String, CheckpointError> {
    match object.remove(name) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(value)) if is_rfc3339(&value) => Ok(value),
        Some(_) => Err(CheckpointError::InvalidField {
            field: field.to_owned(),
            expected: "RFC3339 string",
        }),
    }
}

fn take_optional_integer(
    object: &mut Map<String, Value>,
    name: &str,
) -> Result<(), CheckpointError> {
    match object.remove(name) {
        None | Some(Value::Null) => Ok(()),
        Some(value) if value.as_i64().is_some() => Ok(()),
        Some(_) => Err(CheckpointError::InvalidField {
            field: name.to_owned(),
            expected: "integer",
        }),
    }
}

/// Extracts a Go `bool` field with `omitempty`: absent/null defaults to
/// Go's zero value (`false`).
fn take_bool(object: &mut Map<String, Value>, name: &str) -> Result<bool, CheckpointError> {
    match object.remove(name) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(value),
        Some(_) => Err(CheckpointError::InvalidField {
            field: name.to_owned(),
            expected: "boolean",
        }),
    }
}

/// Extracts a Go `int`/`time.Duration` field with `omitempty`: absent/null
/// defaults to Go's zero value (`0`).
fn take_i64(object: &mut Map<String, Value>, name: &str) -> Result<i64, CheckpointError> {
    match object.remove(name) {
        None | Some(Value::Null) => Ok(0),
        Some(value) => value.as_i64().ok_or_else(|| CheckpointError::InvalidField {
            field: name.to_owned(),
            expected: "signed integer",
        }),
    }
}

/// Extracts a Go `float64` field with `omitempty`: absent/null defaults to
/// Go's zero value (`0.0`).
fn take_f64(object: &mut Map<String, Value>, name: &str) -> Result<f64, CheckpointError> {
    match object.remove(name) {
        None | Some(Value::Null) => Ok(0.0),
        Some(value) => value.as_f64().ok_or_else(|| CheckpointError::InvalidField {
            field: name.to_owned(),
            expected: "number",
        }),
    }
}

fn object_to_btree(object: &Map<String, Value>) -> BTreeMap<String, Value> {
    object
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

struct CurrentCheckpoint<'a>(&'a CheckpointProjection);

impl serde::Serialize for CurrentCheckpoint<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(None)?;
        for (key, value) in &self.0.envelope_extra {
            if key != "version" && key != "payload" {
                map.serialize_entry(key, value)?;
            }
        }
        map.serialize_entry("version", &CHECKPOINT_ENVELOPE_VERSION)?;
        map.serialize_entry("payload", &CurrentPayload(self.0))?;
        map.end()
    }
}

struct CurrentPayload<'a>(&'a CheckpointProjection);

impl serde::Serialize for CurrentPayload<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = self.0;
        let mut map = serializer.serialize_map(None)?;
        for (key, item) in &value.extra {
            if !checkpoint_payload_reserved(key) {
                map.serialize_entry(key, item)?;
            }
        }
        map.serialize_entry("version", &CHECKPOINT_PAYLOAD_VERSION)?;
        map.serialize_entry("workspace_id", &value.workspace_id)?;
        map.serialize_entry("domain", &value.domain)?;
        match &value.plan {
            Some(plan) => map.serialize_entry("plan", &CurrentPlan(plan))?,
            None => map.serialize_entry("plan", &Value::Null)?,
        }
        map.serialize_entry("status", &value.status)?;
        let started_at = if value.started_at.is_empty() {
            GO_ZERO_TIME
        } else {
            &value.started_at
        };
        map.serialize_entry("started_at", started_at)?;
        serialize_non_empty(&mut map, "git_repo_root", &value.git_repo_root)?;
        serialize_non_empty(&mut map, "worktree_path", &value.worktree_path)?;
        serialize_non_empty(&mut map, "branch_name", &value.branch_name)?;
        serialize_non_empty(&mut map, "base_branch", &value.base_branch)?;
        map.end()
    }
}

fn serialize_non_empty<M>(map: &mut M, key: &str, value: &str) -> Result<(), M::Error>
where
    M: SerializeMap,
{
    if !value.is_empty() {
        map.serialize_entry(key, value)?;
    }
    Ok(())
}

/// Mirrors Go's `omitempty` for a slice-typed field: a Go slice (nil or
/// non-nil) with `omitempty` is omitted whenever its length is zero.
fn serialize_non_empty_strings<M>(map: &mut M, key: &str, value: &[String]) -> Result<(), M::Error>
where
    M: SerializeMap,
{
    if !value.is_empty() {
        map.serialize_entry(key, value)?;
    }
    Ok(())
}

/// Mirrors Go's `omitempty` for an `int`/`time.Duration`-typed field: the
/// key is omitted only when the value is Go's zero value (`0`).
fn serialize_non_zero_i64<M>(map: &mut M, key: &str, value: i64) -> Result<(), M::Error>
where
    M: SerializeMap,
{
    if value != 0 {
        map.serialize_entry(key, &value)?;
    }
    Ok(())
}

/// Mirrors Go's `omitempty` for a `float64`-typed field: the key is omitted
/// only when the value is Go's zero value (`0.0`). Exact-zero comparison is
/// intentional here — it matches Go's `omitempty` check, not a precision
/// bug.
#[allow(clippy::float_cmp)]
fn serialize_non_zero_f64<M>(map: &mut M, key: &str, value: f64) -> Result<(), M::Error>
where
    M: SerializeMap,
{
    if value != 0.0 {
        map.serialize_entry(key, &value)?;
    }
    Ok(())
}

/// Mirrors Go's `omitempty` for a `bool`-typed field: the key is omitted
/// whenever the value is Go's zero value (`false`).
fn serialize_if_true<M>(map: &mut M, key: &str, value: bool) -> Result<(), M::Error>
where
    M: SerializeMap,
{
    if value {
        map.serialize_entry(key, &true)?;
    }
    Ok(())
}

struct CurrentPlan<'a>(&'a CheckpointPlan);

impl serde::Serialize for CurrentPlan<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = self.0;
        let mut map = serializer.serialize_map(None)?;
        for (key, item) in &value.extra {
            if !plan_reserved(key) {
                map.serialize_entry(key, item)?;
            }
        }
        map.serialize_entry("id", &value.id)?;
        map.serialize_entry("task", &value.task)?;
        let phases = value.phases.iter().map(CurrentPhase).collect::<Vec<_>>();
        map.serialize_entry("phases", &phases)?;
        map.serialize_entry("execution_mode", &value.execution_mode)?;
        map.serialize_entry("decomp_source", &value.decomp_source)?;
        let created_at = if value.created_at.is_empty() {
            GO_ZERO_TIME
        } else {
            &value.created_at
        };
        map.serialize_entry("created_at", created_at)?;
        map.end()
    }
}

struct CurrentPhase<'a>(&'a CheckpointPhase);

impl serde::Serialize for CurrentPhase<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = self.0;
        let mut map = serializer.serialize_map(None)?;
        for (key, item) in &value.extra {
            if !phase_reserved(key) {
                map.serialize_entry(key, item)?;
            }
        }
        map.serialize_entry("id", &value.id)?;
        map.serialize_entry("name", &value.name)?;
        map.serialize_entry("objective", &value.objective)?;
        map.serialize_entry("persona", &value.persona)?;
        map.serialize_entry("model_tier", &value.model_tier)?;
        map.serialize_entry("skills", &value.skills)?;
        map.serialize_entry("constraints", &value.constraints)?;
        map.serialize_entry("dependencies", &value.dependencies)?;
        map.serialize_entry("expected", &value.expected)?;
        serialize_non_empty(&mut map, "role", &value.role)?;
        serialize_non_empty(&mut map, "target_dir", &value.target_dir)?;
        serialize_non_empty(&mut map, "priority", &value.priority)?;
        serialize_non_empty(&mut map, "runtime", &value.runtime)?;
        serialize_if_true(
            &mut map,
            "runtime_policy_applied",
            value.runtime_policy_applied,
        )?;
        serialize_non_zero_i64(&mut map, "stall_timeout", value.stall_timeout)?;
        map.serialize_entry("status", &value.status)?;
        serialize_non_empty(&mut map, "output", &value.output)?;
        serialize_non_empty(&mut map, "error", &value.error)?;
        serialize_non_empty(&mut map, "start_time", &value.start_time)?;
        serialize_non_empty(&mut map, "end_time", &value.end_time)?;
        serialize_non_empty(&mut map, "signal_remainder", &value.signal_remainder)?;
        serialize_non_zero_i64(&mut map, "retries", value.retries)?;
        serialize_if_true(&mut map, "gate_passed", value.gate_passed)?;
        serialize_non_zero_i64(&mut map, "output_len", value.output_len)?;
        serialize_non_empty_strings(&mut map, "parsed_skills", &value.parsed_skills)?;
        serialize_non_zero_i64(&mut map, "learnings_retrieved", value.learnings_retrieved)?;
        serialize_non_empty(&mut map, "session_id", &value.session_id)?;
        serialize_non_empty(
            &mut map,
            "persona_selection_method",
            &value.persona_selection_method,
        )?;
        serialize_non_empty(&mut map, "model", &value.model)?;
        serialize_non_zero_i64(&mut map, "tokens_in", value.tokens_in)?;
        serialize_non_zero_i64(&mut map, "tokens_out", value.tokens_out)?;
        serialize_non_zero_i64(
            &mut map,
            "tokens_cache_creation",
            value.tokens_cache_creation,
        )?;
        serialize_non_zero_i64(&mut map, "tokens_cache_read", value.tokens_cache_read)?;
        serialize_non_zero_f64(&mut map, "cost_usd", value.cost_usd)?;
        serialize_non_zero_i64(&mut map, "review_iteration", value.review_iteration)?;
        serialize_non_empty(&mut map, "origin_phase_id", &value.origin_phase_id)?;
        serialize_non_zero_i64(&mut map, "max_review_loops", value.max_review_loops)?;
        serialize_non_zero_i64(&mut map, "parse_retry_count", value.parse_retry_count)?;
        serialize_non_empty_strings(&mut map, "review_blockers", &value.review_blockers)?;
        serialize_non_empty_strings(&mut map, "review_warnings", &value.review_warnings)?;
        serialize_non_empty_strings(&mut map, "changed_files", &value.changed_files)?;
        serialize_non_zero_i64(&mut map, "output_bytes", value.output_bytes)?;
        serialize_non_empty(&mut map, "worker", &value.worker)?;
        serialize_non_zero_i64(&mut map, "barok_applied", value.barok_applied)?;
        serialize_non_zero_i64(&mut map, "barok_retry", value.barok_retry)?;
        serialize_non_zero_i64(&mut map, "barok_validator_ms", value.barok_validator_ms)?;
        serialize_non_zero_i64(
            &mut map,
            "barok_retry_validator_ms",
            value.barok_retry_validator_ms,
        )?;
        serialize_non_empty(
            &mut map,
            "barok_first_run_session_id",
            &value.barok_first_run_session_id,
        )?;
        map.end()
    }
}

fn checkpoint_payload_reserved(key: &str) -> bool {
    matches!(
        key,
        "version"
            | "workspace_id"
            | "domain"
            | "plan"
            | "status"
            | "started_at"
            | "git_repo_root"
            | "worktree_path"
            | "branch_name"
            | "base_branch"
            | "linear_issue_id"
            | "mission_path"
    )
}

fn plan_reserved(key: &str) -> bool {
    matches!(
        key,
        "id" | "task" | "phases" | "execution_mode" | "decomp_source" | "created_at"
    )
}

fn phase_reserved(key: &str) -> bool {
    matches!(
        key,
        "id" | "name"
            | "objective"
            | "persona"
            | "model_tier"
            | "skills"
            | "constraints"
            | "dependencies"
            | "expected"
            | "role"
            | "target_dir"
            | "priority"
            | "runtime"
            | "runtime_policy_applied"
            | "stall_timeout"
            | "status"
            | "output"
            | "error"
            | "start_time"
            | "end_time"
            | "signal_remainder"
            | "retries"
            | "gate_passed"
            | "output_len"
            | "parsed_skills"
            | "learnings_retrieved"
            | "session_id"
            | "persona_selection_method"
            | "model"
            | "tokens_in"
            | "tokens_out"
            | "tokens_cache_creation"
            | "tokens_cache_read"
            | "cost_usd"
            | "review_iteration"
            | "origin_phase_id"
            | "max_review_loops"
            | "parse_retry_count"
            | "review_blockers"
            | "review_warnings"
            | "changed_files"
            | "output_bytes"
            | "worker"
            | "barok_applied"
            | "barok_retry"
            | "barok_validator_ms"
            | "barok_retry_validator_ms"
            | "barok_first_run_session_id"
    )
}

/// All stable event type strings in their normative order.
pub const STABLE_EVENT_TYPES: &[&str] = &[
    "mission.started",
    "mission.completed",
    "mission.failed",
    "mission.cancelled",
    "phase.started",
    "phase.completed",
    "phase.failed",
    "phase.skipped",
    "phase.retrying",
    "phase.escalated",
    "worker.spawned",
    "worker.output",
    "worker.completed",
    "worker.failed",
    "ensemble.advisor.started",
    "ensemble.advisor.completed",
    "ensemble.advisor.failed",
    "decompose.started",
    "decompose.completed",
    "decompose.fallback",
    "learning.extracted",
    "learning.stored",
    "learning.injected",
    "dag.dependency_resolved",
    "dag.phase_dispatched",
    "role.handoff",
    "contract.validated",
    "contract.violated",
    "persona.contract_violation",
    "review.findings_emitted",
    "review.external_requested",
    "git.worktree_created",
    "git.committed",
    "git.pr_created",
    "system.error",
    "system.checkpoint_saved",
    "signal.scope_expansion",
    "signal.replan_required",
    "signal.human_decision_needed",
    "file_overlap.detected",
    "security.invisible_chars_stripped",
    "security.injection_detected",
    "zettel.written",
    "zettel.skipped",
    "zettel.write_failed",
];

/// A current or forward event envelope.
///
/// Recursive JSON fields use [`EventJsonMap`], whose ownership operations tear
/// down nested [`serde_json::Value`] containers iteratively. `EventRecord`
/// itself therefore needs no destructor, so callers may move or destructure
/// any owned field normally.
#[derive(Clone, Debug, PartialEq)]
pub struct EventRecord {
    pub id: String,
    pub event_type: String,
    pub timestamp: String,
    pub sequence: i64,
    pub mission_id: String,
    pub phase_id: Option<String>,
    pub worker_id: Option<String>,
    pub data: Option<EventJsonMap>,
    pub extra: EventJsonMap,
}

/// Stack-safe owner of a JSON object used by an event envelope.
///
/// The raw map remains private. Borrowed inspection and controlled mutation do
/// not expose ownership of nested values, so replacement and destruction
/// always use iterative teardown.
#[derive(Default)]
pub struct EventJsonMap {
    values: BTreeMap<String, Value>,
}

impl EventJsonMap {
    /// Returns one value by key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    /// Returns whether the map contains `key`.
    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    /// Iterates over entries in deterministic key order.
    pub fn iter(&self) -> std::collections::btree_map::Iter<'_, String, Value> {
        self.values.iter()
    }

    /// Returns the number of top-level entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns whether the map has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Inserts a value and iteratively destroys any value it replaces.
    ///
    /// Returns `true` when an existing entry was replaced.
    pub fn insert(&mut self, key: String, value: Value) -> bool {
        let replaced = self.values.insert(key, value);
        let did_replace = replaced.is_some();
        if let Some(replaced) = replaced {
            drop_json_value_iteratively(replaced);
        }
        did_replace
    }

    /// Extends the map, iteratively destroying values replaced by duplicate
    /// keys.
    pub fn extend(&mut self, entries: impl IntoIterator<Item = (String, Value)>) {
        for (key, value) in entries {
            self.insert(key, value);
        }
    }

    /// Removes and iteratively destroys one value.
    ///
    /// Returns `true` when the key was present.
    pub fn remove_and_drop(&mut self, key: &str) -> bool {
        let removed = self.values.remove(key);
        let did_remove = removed.is_some();
        if let Some(removed) = removed {
            drop_json_value_iteratively(removed);
        }
        did_remove
    }

    /// Removes every entry with iterative value teardown.
    pub fn clear(&mut self) {
        drop_json_map_iteratively(std::mem::take(&mut self.values));
    }

    /// Replaces every entry and iteratively destroys the prior map.
    pub fn replace_with(&mut self, replacement: BTreeMap<String, Value>) {
        let previous = std::mem::replace(&mut self.values, replacement);
        drop_json_map_iteratively(previous);
    }
}

impl From<BTreeMap<String, Value>> for EventJsonMap {
    fn from(values: BTreeMap<String, Value>) -> Self {
        Self { values }
    }
}

impl FromIterator<(String, Value)> for EventJsonMap {
    fn from_iter<T: IntoIterator<Item = (String, Value)>>(iter: T) -> Self {
        let mut values = Self::default();
        values.extend(iter);
        values
    }
}

impl Clone for EventJsonMap {
    fn clone(&self) -> Self {
        self.values
            .iter()
            .map(|(key, value)| (key.clone(), clone_json_value_iteratively(value)))
            .collect()
    }
}

impl PartialEq for EventJsonMap {
    fn eq(&self, other: &Self) -> bool {
        self.values.len() == other.values.len()
            && self.values.iter().zip(&other.values).all(
                |((left_key, left), (right_key, right))| {
                    left_key == right_key && json_values_equal_iteratively(left, right)
                },
            )
    }
}

impl Eq for EventJsonMap {}

impl fmt::Debug for EventJsonMap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        format_json_map_debug_iteratively(&self.values, formatter)
    }
}

impl Drop for EventJsonMap {
    fn drop(&mut self) {
        drop_json_map_iteratively(std::mem::take(&mut self.values));
    }
}

fn drop_json_map_iteratively(values: BTreeMap<String, Value>) {
    drop_json_values_iteratively(values.into_values().collect());
}

fn drop_json_value_iteratively(value: Value) {
    drop_json_values_iteratively(vec![value]);
}

fn drop_json_values_iteratively(mut pending: Vec<Value>) {
    while let Some(value) = pending.pop() {
        match value {
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => {
                pending.extend(values.into_iter().map(|(_, value)| value));
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
}

enum JsonCloneFrame<'a> {
    Value(&'a Value),
    Array {
        result_start: usize,
    },
    Object {
        result_start: usize,
        keys: Vec<&'a String>,
    },
}

fn clone_json_value_iteratively(root: &Value) -> Value {
    let mut pending = vec![JsonCloneFrame::Value(root)];
    let mut results = Vec::new();
    while let Some(frame) = pending.pop() {
        match frame {
            JsonCloneFrame::Value(value) => match value {
                Value::Null => results.push(Value::Null),
                Value::Bool(value) => results.push(Value::Bool(*value)),
                Value::Number(value) => results.push(Value::Number(value.clone())),
                Value::String(value) => results.push(Value::String(value.clone())),
                Value::Array(values) => {
                    pending.push(JsonCloneFrame::Array {
                        result_start: results.len(),
                    });
                    pending.extend(values.iter().rev().map(JsonCloneFrame::Value));
                }
                Value::Object(values) => {
                    let entries: Vec<_> = values.iter().collect();
                    pending.push(JsonCloneFrame::Object {
                        result_start: results.len(),
                        keys: entries.iter().map(|(key, _)| *key).collect(),
                    });
                    pending.extend(
                        entries
                            .into_iter()
                            .rev()
                            .map(|(_, value)| JsonCloneFrame::Value(value)),
                    );
                }
            },
            JsonCloneFrame::Array { result_start } => {
                let values = results.split_off(result_start);
                results.push(Value::Array(values));
            }
            JsonCloneFrame::Object { result_start, keys } => {
                let values = results.split_off(result_start);
                let object = keys
                    .into_iter()
                    .zip(values)
                    .map(|(key, value)| (key.clone(), value))
                    .collect();
                results.push(Value::Object(object));
            }
        }
    }
    results.pop().unwrap_or(Value::Null)
}

fn json_values_equal_iteratively(left: &Value, right: &Value) -> bool {
    let mut pending = vec![(left, right)];
    while let Some((left, right)) = pending.pop() {
        match (left, right) {
            (Value::Null, Value::Null) => {}
            (Value::Bool(left), Value::Bool(right)) if left == right => {}
            (Value::Number(left), Value::Number(right)) if left == right => {}
            (Value::String(left), Value::String(right)) if left == right => {}
            (Value::Array(left), Value::Array(right)) if left.len() == right.len() => {
                pending.extend(left.iter().zip(right));
            }
            (Value::Object(left), Value::Object(right)) if left.len() == right.len() => {
                for ((left_key, left), (right_key, right)) in left.iter().zip(right) {
                    if left_key != right_key {
                        return false;
                    }
                    pending.push((left, right));
                }
            }
            _ => return false,
        }
    }
    true
}

enum JsonDebugFrame<'a> {
    Value(&'a Value),
    Key(&'a str),
    Text(&'static str),
}

fn push_json_object_debug_frames<'a>(
    pending: &mut Vec<JsonDebugFrame<'a>>,
    entries: impl Iterator<Item = (&'a String, &'a Value)>,
    prefix: &'static str,
) {
    let entries: Vec<_> = entries.collect();
    pending.push(JsonDebugFrame::Text("}"));
    for (index, (key, value)) in entries.into_iter().enumerate().rev() {
        pending.push(JsonDebugFrame::Value(value));
        pending.push(JsonDebugFrame::Text(": "));
        pending.push(JsonDebugFrame::Key(key));
        if index > 0 {
            pending.push(JsonDebugFrame::Text(", "));
        }
    }
    pending.push(JsonDebugFrame::Text(prefix));
}

fn push_json_array_debug_frames<'a>(pending: &mut Vec<JsonDebugFrame<'a>>, values: &'a [Value]) {
    pending.push(JsonDebugFrame::Text("]"));
    for (index, value) in values.iter().enumerate().rev() {
        pending.push(JsonDebugFrame::Value(value));
        if index > 0 {
            pending.push(JsonDebugFrame::Text(", "));
        }
    }
    pending.push(JsonDebugFrame::Text("Array ["));
}

fn format_json_map_debug_iteratively(
    values: &BTreeMap<String, Value>,
    formatter: &mut fmt::Formatter<'_>,
) -> fmt::Result {
    let mut pending = Vec::new();
    push_json_object_debug_frames(&mut pending, values.iter(), "{");
    while let Some(frame) = pending.pop() {
        match frame {
            JsonDebugFrame::Text(value) => formatter.write_str(value)?,
            JsonDebugFrame::Key(value) => fmt::Debug::fmt(value, formatter)?,
            JsonDebugFrame::Value(Value::Null) => formatter.write_str("Null")?,
            JsonDebugFrame::Value(Value::Bool(value)) => {
                write!(formatter, "Bool({value})")?;
            }
            JsonDebugFrame::Value(Value::Number(value)) => {
                fmt::Debug::fmt(value, formatter)?;
            }
            JsonDebugFrame::Value(Value::String(value)) => {
                formatter.write_str("String(")?;
                fmt::Debug::fmt(value, formatter)?;
                formatter.write_str(")")?;
            }
            JsonDebugFrame::Value(Value::Array(values)) => {
                push_json_array_debug_frames(&mut pending, values);
            }
            JsonDebugFrame::Value(Value::Object(values)) => {
                push_json_object_debug_frames(&mut pending, values.iter(), "Object {");
            }
        }
    }
    Ok(())
}

/// Parsed event plus exact source bytes for preservation replay.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedEvent {
    pub record: EventRecord,
    pub is_stable_type: bool,
    pub raw_line: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventScanDiagnosticKind {
    Corrupt,
    Oversized,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventScanDiagnostic {
    pub line_index: usize,
    pub kind: EventScanDiagnosticKind,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EventScan {
    pub events: Vec<DecodedEvent>,
    pub diagnostics: Vec<EventScanDiagnostic>,
    /// The next sequence, or `None` when the greatest valid sequence is `i64::MAX`.
    pub next_sequence: Option<i64>,
}

#[derive(Debug, Error)]
pub enum EventError {
    #[error("event is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("event is not valid JSON: {0}")]
    GoJson(&'static str),
    #[error("event root must be an object")]
    RootNotObject,
    #[error("event field {field} is missing or invalid; expected {expected}")]
    InvalidField {
        field: &'static str,
        expected: &'static str,
    },
    #[error("event timestamp is not RFC3339: {0:?}")]
    InvalidTimestamp(String),
    #[error("event data must be an object when present")]
    InvalidData,
}

/// Decodes one version-one event envelope and retains its original bytes.
pub fn decode_event_line(bytes: &[u8]) -> Result<DecodedEvent, EventError> {
    let value: Value = serde_json::from_slice(bytes)?;
    let mut object = value
        .as_object()
        .cloned()
        .ok_or(EventError::RootNotObject)?;
    let id = required_event_string(&mut object, "id")?;
    let event_type = required_event_string(&mut object, "type")?;
    let timestamp = required_event_string(&mut object, "timestamp")?;
    if !is_rfc3339(&timestamp) {
        return Err(EventError::InvalidTimestamp(timestamp));
    }
    let sequence = object
        .remove("sequence")
        .and_then(|value| value.as_i64())
        .ok_or(EventError::InvalidField {
            field: "sequence",
            expected: "signed integer",
        })?;
    let mission_id = required_event_string(&mut object, "mission_id")?;
    let phase_id = optional_event_string(&mut object, "phase_id")?;
    let worker_id = optional_event_string(&mut object, "worker_id")?;
    let data = match object.remove("data") {
        None | Some(Value::Null) => None,
        Some(Value::Object(data)) => Some(data.into_iter().collect::<EventJsonMap>()),
        Some(_) => return Err(EventError::InvalidData),
    };
    let is_stable_type = STABLE_EVENT_TYPES.contains(&event_type.as_str());
    Ok(DecodedEvent {
        record: EventRecord {
            id,
            event_type,
            timestamp,
            sequence,
            mission_id,
            phase_id,
            worker_id,
            data,
            extra: object.into_iter().collect::<EventJsonMap>(),
        },
        is_stable_type,
        raw_line: bytes.to_vec(),
    })
}

/// Decodes an event exactly as Go's read-only `json.Unmarshal` paths observe
/// its `Event` struct. Missing fields retain Go zero values; explicit-null
/// scalar fields leave their current value untouched (initially zero), while
/// wrong-typed known fields, malformed timestamps, malformed JSON, and
/// non-object `data` still fail.
///
/// This decoder is intentionally separate from [`decode_event_line`]. It is
/// suitable for compatibility display, projection, and sequence observation;
/// it must not be used to admit authenticated or newly persisted events.
pub fn decode_go_observed_event_line(bytes: &[u8]) -> Result<DecodedEvent, EventError> {
    let record = parse_go_observed_event(bytes, true)?;
    let is_stable_type = STABLE_EVENT_TYPES.contains(&record.event_type.as_str());
    Ok(DecodedEvent {
        record,
        is_stable_type,
        raw_line: bytes.to_vec(),
    })
}

/// Decodes the fields Go observes without copying the source line or retaining
/// ignored top-level fields. Streaming projection and sequence readers should
/// prefer this surface when they do not need preservation replay.
pub fn decode_go_observed_event_record(bytes: &[u8]) -> Result<EventRecord, EventError> {
    parse_go_observed_event(bytes, false)
}

/// One source-ordered member from a Go-compatible JSON object projection.
///
/// `raw_value` borrows the exact value bytes, while `name` has Go's
/// `encoding/json` UTF-8 replacement and string-unescaping behavior applied.
/// Duplicate names are retained in source order so a struct projection can
/// apply Go's field-specific merge/last-write behavior.
#[derive(Debug, PartialEq)]
pub struct GoJsonObjectMember<'a> {
    pub name: String,
    pub raw_value: &'a [u8],
}

/// A scalar decoded with Go's `encoding/json` string behavior.
#[derive(Debug, PartialEq)]
pub enum GoJsonScalar<'a> {
    Null,
    Bool(bool),
    Number(&'a str),
    String {
        decoded: String,
        /// Exact bytes between the JSON quotes. Time-like types use these to
        /// preserve Go's rejection of JSON escapes in `Time.UnmarshalJSON`.
        raw_content: &'a [u8],
    },
}

/// Tokenizes a JSON object without collapsing duplicate members.
///
/// `null` returns `None`, matching unmarshalling into an existing Go struct:
/// it leaves the destination unchanged. Non-object roots return
/// [`EventError::RootNotObject`]. Nested values are fully validated by the
/// same iterative parser used by [`decode_go_observed_event_line`].
pub fn decode_go_json_object_members(
    bytes: &[u8],
) -> Result<Option<Vec<GoJsonObjectMember<'_>>>, EventError> {
    let mut parser = GoJsonParser::new(bytes);
    parser.skip_whitespace();
    if parser.consume_null()? {
        parser.finish()?;
        return Ok(None);
    }
    if parser.peek() != Some(b'{') {
        parser.skip_value(0)?;
        parser.finish()?;
        return Err(EventError::RootNotObject);
    }

    parser.expect(b'{')?;
    parser.skip_whitespace();
    let mut members = Vec::new();
    if parser.peek() == Some(b'}') {
        parser.cursor += 1;
        parser.finish()?;
        return Ok(Some(members));
    }

    loop {
        let name = parser.parse_string()?;
        parser.skip_whitespace();
        parser.expect(b':')?;
        parser.skip_whitespace();
        let value_start = parser.cursor;
        parser.skip_value(1)?;
        members.push(GoJsonObjectMember {
            name,
            raw_value: &bytes[value_start..parser.cursor],
        });
        parser.skip_whitespace();
        match parser.peek() {
            Some(b',') => {
                parser.cursor += 1;
                parser.skip_whitespace();
            }
            Some(b'}') => {
                parser.cursor += 1;
                parser.finish()?;
                return Ok(Some(members));
            }
            _ => return Err(EventError::GoJson("expected object delimiter")),
        }
    }
}

/// Tokenizes a JSON array into exact, source-ordered element slices.
///
/// `null` returns `None`; the caller applies the destination type's Go null
/// semantics. Every nested element is validated by the iterative
/// compatibility parser.
pub fn decode_go_json_array_elements(bytes: &[u8]) -> Result<Option<Vec<&[u8]>>, EventError> {
    let mut parser = GoJsonParser::new(bytes);
    parser.skip_whitespace();
    if parser.consume_null()? {
        parser.finish()?;
        return Ok(None);
    }
    if parser.peek() != Some(b'[') {
        parser.skip_value(0)?;
        parser.finish()?;
        return Err(EventError::InvalidData);
    }

    parser.expect(b'[')?;
    parser.skip_whitespace();
    let mut elements = Vec::new();
    if parser.peek() == Some(b']') {
        parser.cursor += 1;
        parser.finish()?;
        return Ok(Some(elements));
    }

    loop {
        let value_start = parser.cursor;
        parser.skip_value(1)?;
        elements.push(&bytes[value_start..parser.cursor]);
        parser.skip_whitespace();
        match parser.peek() {
            Some(b',') => {
                parser.cursor += 1;
                parser.skip_whitespace();
            }
            Some(b']') => {
                parser.cursor += 1;
                parser.finish()?;
                return Ok(Some(elements));
            }
            _ => return Err(EventError::GoJson("expected array delimiter")),
        }
    }
}

/// Decodes one JSON scalar while retaining a string's raw quoted content.
pub fn decode_go_json_scalar(bytes: &[u8]) -> Result<GoJsonScalar<'_>, EventError> {
    let mut parser = GoJsonParser::new(bytes);
    parser.skip_whitespace();
    let scalar = match parser.peek() {
        Some(b'n') => {
            parser.parse_literal(b"null")?;
            GoJsonScalar::Null
        }
        Some(b't') => {
            parser.parse_literal(b"true")?;
            GoJsonScalar::Bool(true)
        }
        Some(b'f') => {
            parser.parse_literal(b"false")?;
            GoJsonScalar::Bool(false)
        }
        Some(b'"') => {
            let start = parser.cursor;
            let decoded = parser.parse_string()?;
            GoJsonScalar::String {
                decoded,
                raw_content: &bytes[start + 1..parser.cursor - 1],
            }
        }
        Some(b'-' | b'0'..=b'9') => {
            let range = parser.parse_number_range()?;
            let number = std::str::from_utf8(&bytes[range]).map_err(|_| EventError::InvalidData)?;
            GoJsonScalar::Number(number)
        }
        Some(_) => {
            parser.skip_value(0)?;
            parser.finish()?;
            return Err(EventError::InvalidData);
        }
        None => return Err(EventError::GoJson("empty input")),
    };
    parser.finish()?;
    Ok(scalar)
}

/// Matches a decoded JSON member name to an ASCII struct tag using Go's
/// `encoding/json` exact-or-Unicode-simple-fold behavior.
#[must_use]
pub fn go_json_field_matches(name: &str, ascii_tag: &str) -> bool {
    name == ascii_tag || go_equal_fold_ascii_tag(name, ascii_tag)
}

/// Scans JSONL in file order, skipping corrupt and over-limit lines.
#[must_use]
pub fn scan_event_log(bytes: &[u8]) -> EventScan {
    let mut events = Vec::new();
    let mut diagnostics = Vec::new();
    let mut greatest: Option<i64> = None;
    let mut start = 0;
    let mut line_index = 1;
    while start < bytes.len() {
        let end = bytes[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |offset| start + offset + 1);
        let raw = &bytes[start..end];
        let content = raw.strip_suffix(b"\n").unwrap_or(raw);
        let content = content.strip_suffix(b"\r").unwrap_or(content);
        if content.len() > GO_EVENT_JSON_CONTENT_MAX_BYTES {
            diagnostics.push(EventScanDiagnostic {
                line_index,
                kind: EventScanDiagnosticKind::Oversized,
                message: format!("line exceeds {GO_EVENT_JSON_CONTENT_MAX_BYTES} bytes"),
            });
        } else {
            match decode_event_line(content) {
                Ok(mut event) => {
                    event.raw_line = raw.to_vec();
                    greatest = Some(greatest.map_or(event.record.sequence, |current| {
                        current.max(event.record.sequence)
                    }));
                    events.push(event);
                }
                Err(error) => diagnostics.push(EventScanDiagnostic {
                    line_index,
                    kind: EventScanDiagnosticKind::Corrupt,
                    message: error.to_string(),
                }),
            }
        }
        start = end;
        line_index += 1;
    }
    EventScan {
        events,
        diagnostics,
        next_sequence: greatest.map_or(Some(1), |sequence| sequence.checked_add(1)),
    }
}

/// Encodes a compact current event object without a trailing newline.
///
/// The frozen Go snapshot's canonical event-log writer,
/// `FileEmitter.Emit` (`internal/event/file.go:54`, `data, err :=
/// json.Marshal(ev)`), uses plain `encoding/json.Marshal` with no
/// `Encoder.SetEscapeHTML(false)` anywhere in the snapshot (confirmed by
/// grep), so it HTML-escapes `<`, `>`, `&`, U+2028, and U+2029
/// unconditionally exactly like the checkpoint/plan writers. The same
/// `escape_go_json_for_html` post-processing step is applied here for
/// consistency with `encode_current_checkpoint`/`encode_current_plan`.
pub fn encode_current_event(value: &EventRecord) -> Result<Vec<u8>, EventError> {
    validate_event_record(value)?;
    let json = serde_json::to_string(&CurrentEvent(value))?;
    Ok(escape_go_json_for_html(&json).into_bytes())
}

/// Matches `encoding/json`'s unconditional HTML escaping applied by
/// `Marshal`/`MarshalIndent` (and by any `Encoder` that has not called
/// `SetEscapeHTML(false)`). These code points cannot occur as JSON
/// structural syntax, so replacing them in the completed document affects
/// string values only. Mirrors
/// `orchestrator-app/src/audit_read.rs`'s `escape_go_json_for_html`
/// (`orchestrator-core` cannot depend on `orchestrator-app`, so the
/// function is duplicated rather than shared).
fn escape_go_json_for_html(json: &str) -> String {
    let mut escaped = String::with_capacity(json.len());
    for character in json.chars() {
        match character {
            '<' => escaped.push_str(r"\u003c"),
            '>' => escaped.push_str(r"\u003e"),
            '&' => escaped.push_str(r"\u0026"),
            '\u{2028}' => escaped.push_str(r"\u2028"),
            '\u{2029}' => escaped.push_str(r"\u2029"),
            _ => escaped.push(character),
        }
    }
    escaped
}

/// Returns the exact valid line bytes retained during decoding or scanning.
#[must_use]
pub fn encode_preserved_event(value: &DecodedEvent) -> Vec<u8> {
    value.raw_line.clone()
}

fn required_event_string(
    object: &mut Map<String, Value>,
    name: &'static str,
) -> Result<String, EventError> {
    match object.remove(name) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value),
        _ => Err(EventError::InvalidField {
            field: name,
            expected: "non-empty string",
        }),
    }
}

fn optional_event_string(
    object: &mut Map<String, Value>,
    name: &'static str,
) -> Result<Option<String>, EventError> {
    match object.remove(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(EventError::InvalidField {
            field: name,
            expected: "string",
        }),
    }
}

#[derive(Clone, Copy)]
enum GoObservedField {
    Id,
    Type,
    Timestamp,
    Sequence,
    MissionId,
    PhaseId,
    WorkerId,
    Data,
}

const GO_OBSERVED_FIELDS: [(&str, GoObservedField); 8] = [
    ("id", GoObservedField::Id),
    ("type", GoObservedField::Type),
    ("timestamp", GoObservedField::Timestamp),
    ("sequence", GoObservedField::Sequence),
    ("mission_id", GoObservedField::MissionId),
    ("phase_id", GoObservedField::PhaseId),
    ("worker_id", GoObservedField::WorkerId),
    ("data", GoObservedField::Data),
];

fn go_observed_field(name: &str) -> Option<GoObservedField> {
    GO_OBSERVED_FIELDS
        .iter()
        .find(|(field_name, _)| *field_name == name)
        .or_else(|| {
            GO_OBSERVED_FIELDS
                .iter()
                .find(|(field_name, _)| go_equal_fold_ascii_tag(name, field_name))
        })
        .map(|(_, field)| *field)
}

struct GoObservedState {
    id: String,
    event_type: String,
    timestamp: String,
    sequence: i64,
    mission_id: String,
    phase_id: String,
    worker_id: String,
    data: Option<EventJsonMap>,
    extra: EventJsonMap,
}

impl GoObservedState {
    fn new() -> Self {
        Self {
            id: String::new(),
            event_type: String::new(),
            timestamp: GO_ZERO_TIME.to_owned(),
            sequence: 0,
            mission_id: String::new(),
            phase_id: String::new(),
            worker_id: String::new(),
            data: None,
            extra: EventJsonMap::default(),
        }
    }

    fn into_record(mut self) -> EventRecord {
        EventRecord {
            id: std::mem::take(&mut self.id),
            event_type: std::mem::take(&mut self.event_type),
            timestamp: std::mem::take(&mut self.timestamp),
            sequence: self.sequence,
            mission_id: std::mem::take(&mut self.mission_id),
            phase_id: non_empty(std::mem::take(&mut self.phase_id)),
            worker_id: non_empty(std::mem::take(&mut self.worker_id)),
            data: self.data.take(),
            extra: std::mem::take(&mut self.extra),
        }
    }
}

fn parse_go_observed_event(bytes: &[u8], retain_extra: bool) -> Result<EventRecord, EventError> {
    let mut parser = GoJsonParser::new(bytes);
    parser.skip_whitespace();
    match parser.peek() {
        Some(b'n') => {
            parser.parse_literal(b"null")?;
            parser.finish()?;
            Ok(GoObservedState::new().into_record())
        }
        Some(b'{') => {
            let state = parser.parse_event_object(retain_extra)?;
            parser.finish()?;
            Ok(state.into_record())
        }
        Some(_) => {
            parser.skip_value(0)?;
            parser.finish()?;
            Err(EventError::RootNotObject)
        }
        None => Err(EventError::GoJson("empty input")),
    }
}

fn go_equal_fold_ascii_tag(value: &str, ascii_tag: &str) -> bool {
    let mut value = value.chars();
    let mut tag = ascii_tag.bytes();
    loop {
        match (value.next(), tag.next()) {
            (None, None) => return true,
            (Some(character), Some(expected)) => {
                let folded = match character {
                    'a'..='z' => character as u8 - b'a' + b'A',
                    '\u{017f}' => b'S', // LATIN SMALL LETTER LONG S
                    '\u{212a}' => b'K', // KELVIN SIGN
                    character if character.is_ascii() => character as u8,
                    _ => return false,
                };
                if folded != expected.to_ascii_uppercase() {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

struct GoJsonParser<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

// encoding/json's scanner counts every open array or object, including the root.
const GO_JSON_MAX_NESTING_DEPTH: usize = 10_000;

enum ValueStart<T> {
    Complete(T),
    Nested,
}

enum ParsedContainer {
    Array(Vec<Value>),
    Object {
        values: Map<String, Value>,
        key: String,
    },
}

impl ParsedContainer {
    fn into_value(self) -> Value {
        match self {
            Self::Array(values) => Value::Array(values),
            Self::Object { values, .. } => Value::Object(values),
        }
    }
}

#[derive(Default)]
struct ParsedContainerStack(Vec<ParsedContainer>);

impl ParsedContainerStack {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn last_mut(&mut self) -> Option<&mut ParsedContainer> {
        self.0.last_mut()
    }

    fn push(&mut self, container: ParsedContainer) {
        self.0.push(container);
    }

    fn pop(&mut self) -> Option<ParsedContainer> {
        self.0.pop()
    }
}

impl Drop for ParsedContainerStack {
    fn drop(&mut self) {
        let mut pending = Vec::new();
        while let Some(container) = self.0.pop() {
            match container {
                ParsedContainer::Array(values) => pending.extend(values),
                ParsedContainer::Object { values, .. } => {
                    pending.extend(values.into_iter().map(|(_, value)| value));
                }
            }
        }
        drop_json_values_iteratively(pending);
    }
}

#[derive(Clone, Copy)]
enum SkippedContainer {
    Array,
    Object,
}

impl<'a> GoJsonParser<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.cursor).copied()
    }

    fn skip_whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.cursor += 1;
        }
    }

    fn finish(&mut self) -> Result<(), EventError> {
        self.skip_whitespace();
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(EventError::GoJson("trailing data"))
        }
    }

    fn expect(&mut self, expected: u8) -> Result<(), EventError> {
        if self.peek() == Some(expected) {
            self.cursor += 1;
            Ok(())
        } else {
            Err(EventError::GoJson("unexpected token"))
        }
    }

    fn parse_literal(&mut self, expected: &[u8]) -> Result<(), EventError> {
        if self.bytes.get(self.cursor..self.cursor + expected.len()) == Some(expected) {
            self.cursor += expected.len();
            Ok(())
        } else {
            Err(EventError::GoJson("invalid literal"))
        }
    }

    fn consume_null(&mut self) -> Result<bool, EventError> {
        if self.peek() == Some(b'n') {
            self.parse_literal(b"null")?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn parse_event_object(&mut self, retain_extra: bool) -> Result<GoObservedState, EventError> {
        self.expect(b'{')?;
        self.skip_whitespace();
        let mut state = GoObservedState::new();
        if self.peek() == Some(b'}') {
            self.cursor += 1;
            return Ok(state);
        }

        loop {
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            match go_observed_field(&key) {
                Some(GoObservedField::Id) => self.parse_nullable_string(&mut state.id, "id")?,
                Some(GoObservedField::Type) => {
                    self.parse_nullable_string(&mut state.event_type, "type")?;
                }
                Some(GoObservedField::Timestamp) => {
                    self.parse_nullable_timestamp(&mut state.timestamp)?;
                }
                Some(GoObservedField::Sequence) => {
                    self.parse_nullable_sequence(&mut state.sequence)?;
                }
                Some(GoObservedField::MissionId) => {
                    self.parse_nullable_string(&mut state.mission_id, "mission_id")?;
                }
                Some(GoObservedField::PhaseId) => {
                    self.parse_nullable_string(&mut state.phase_id, "phase_id")?;
                }
                Some(GoObservedField::WorkerId) => {
                    self.parse_nullable_string(&mut state.worker_id, "worker_id")?;
                }
                Some(GoObservedField::Data) => self.parse_nullable_data(&mut state.data)?,
                None => {
                    let value_start = self.cursor;
                    self.skip_value(1)?;
                    if retain_extra {
                        let mut extra_parser = Self::new(&self.bytes[value_start..self.cursor]);
                        if let Ok(value) = extra_parser.parse_value(0) {
                            if extra_parser.finish().is_ok() {
                                state.extra.insert(key, value);
                            } else {
                                drop_json_value_iteratively(value);
                            }
                        }
                    }
                }
            }

            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.cursor += 1;
                    self.skip_whitespace();
                }
                Some(b'}') => {
                    self.cursor += 1;
                    return Ok(state);
                }
                _ => return Err(EventError::GoJson("expected object delimiter")),
            }
        }
    }

    fn parse_nullable_string(
        &mut self,
        destination: &mut String,
        field: &'static str,
    ) -> Result<(), EventError> {
        if self.consume_null()? {
            return Ok(());
        }
        if self.peek() != Some(b'"') {
            return Err(EventError::InvalidField {
                field,
                expected: "string or null",
            });
        }
        *destination = self.parse_string()?;
        Ok(())
    }

    fn parse_nullable_sequence(&mut self, destination: &mut i64) -> Result<(), EventError> {
        if self.consume_null()? {
            return Ok(());
        }
        let range = self
            .parse_number_range()
            .map_err(|_| EventError::InvalidField {
                field: "sequence",
                expected: "signed integer or null",
            })?;
        let number =
            std::str::from_utf8(&self.bytes[range]).map_err(|_| EventError::InvalidField {
                field: "sequence",
                expected: "signed integer or null",
            })?;
        *destination = number.parse().map_err(|_| EventError::InvalidField {
            field: "sequence",
            expected: "signed integer or null",
        })?;
        Ok(())
    }

    fn parse_nullable_timestamp(&mut self, destination: &mut String) -> Result<(), EventError> {
        if self.consume_null()? {
            return Ok(());
        }
        if self.peek() != Some(b'"') {
            return Err(EventError::InvalidField {
                field: "timestamp",
                expected: "RFC3339 string or null",
            });
        }
        let start = self.cursor;
        self.skip_string()?;
        let raw_value = &self.bytes[start..self.cursor];
        let raw_timestamp = &raw_value[1..raw_value.len() - 1];
        *destination = normalize_go_time_unmarshal_rfc3339(raw_timestamp).ok_or_else(|| {
            EventError::InvalidTimestamp(String::from_utf8_lossy(raw_timestamp).into_owned())
        })?;
        Ok(())
    }

    fn parse_nullable_data(
        &mut self,
        destination: &mut Option<EventJsonMap>,
    ) -> Result<(), EventError> {
        if self.consume_null()? {
            *destination = None;
            return Ok(());
        }
        if self.peek() != Some(b'{') {
            return Err(EventError::InvalidData);
        }
        let Value::Object(value) = self.parse_value(1)? else {
            return Err(EventError::InvalidData);
        };
        if let Some(current) = destination {
            current.extend(value);
        } else {
            *destination = Some(value.into_iter().collect::<EventJsonMap>());
        }
        Ok(())
    }

    fn parse_string(&mut self) -> Result<String, EventError> {
        let start = self.cursor;
        self.skip_string()?;
        decode_go_json_string(&self.bytes[start..self.cursor])
    }

    fn skip_string(&mut self) -> Result<(), EventError> {
        self.expect(b'"')?;
        loop {
            let Some(byte) = self.peek() else {
                return Err(EventError::GoJson("unterminated string"));
            };
            match byte {
                b'"' => {
                    self.cursor += 1;
                    return Ok(());
                }
                b'\\' => {
                    self.cursor += 1;
                    let Some(escape) = self.peek() else {
                        return Err(EventError::GoJson("unterminated escape"));
                    };
                    self.cursor += 1;
                    match escape {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                        b'u' => {
                            let digits = self
                                .bytes
                                .get(self.cursor..self.cursor + 4)
                                .ok_or(EventError::GoJson("short unicode escape"))?;
                            if !digits.iter().all(u8::is_ascii_hexdigit) {
                                return Err(EventError::GoJson("invalid unicode escape"));
                            }
                            self.cursor += 4;
                        }
                        _ => return Err(EventError::GoJson("invalid string escape")),
                    }
                }
                0x00..=0x1f => return Err(EventError::GoJson("control byte in string")),
                _ => self.cursor += 1,
            }
        }
    }

    fn parse_value(&mut self, outer_depth: usize) -> Result<Value, EventError> {
        let mut stack = ParsedContainerStack::default();
        loop {
            let ValueStart::Complete(mut value) =
                self.parse_value_start(outer_depth, &mut stack)?
            else {
                continue;
            };

            loop {
                let Some(container) = stack.last_mut() else {
                    return Ok(value);
                };
                let container_complete = match container {
                    ParsedContainer::Array(values) => {
                        values.push(value);
                        self.skip_whitespace();
                        match self.peek() {
                            Some(b',') => {
                                self.cursor += 1;
                                self.skip_whitespace();
                                false
                            }
                            Some(b']') => {
                                self.cursor += 1;
                                true
                            }
                            _ => {
                                return Err(EventError::GoJson("expected array delimiter"));
                            }
                        }
                    }
                    ParsedContainer::Object { values, key } => {
                        if let Some(replaced) = values.insert(std::mem::take(key), value) {
                            drop_json_value_iteratively(replaced);
                        }
                        self.skip_whitespace();
                        match self.peek() {
                            Some(b',') => {
                                self.cursor += 1;
                                self.skip_whitespace();
                                *key = self.parse_object_key()?;
                                false
                            }
                            Some(b'}') => {
                                self.cursor += 1;
                                true
                            }
                            _ => {
                                return Err(EventError::GoJson("expected object delimiter"));
                            }
                        }
                    }
                };
                if !container_complete {
                    break;
                }
                value = stack
                    .pop()
                    .ok_or(EventError::GoJson("missing value container"))?
                    .into_value();
            }
        }
    }

    fn parse_value_start(
        &mut self,
        outer_depth: usize,
        stack: &mut ParsedContainerStack,
    ) -> Result<ValueStart<Value>, EventError> {
        match self.peek() {
            Some(b'n') => {
                self.parse_literal(b"null")?;
                Ok(ValueStart::Complete(Value::Null))
            }
            Some(b't') => {
                self.parse_literal(b"true")?;
                Ok(ValueStart::Complete(Value::Bool(true)))
            }
            Some(b'f') => {
                self.parse_literal(b"false")?;
                Ok(ValueStart::Complete(Value::Bool(false)))
            }
            Some(b'"') => self
                .parse_string()
                .map(Value::String)
                .map(ValueStart::Complete),
            Some(b'[') => {
                self.begin_container(outer_depth, stack.len(), b'[')?;
                self.skip_whitespace();
                if self.peek() == Some(b']') {
                    self.cursor += 1;
                    Ok(ValueStart::Complete(Value::Array(Vec::new())))
                } else {
                    stack.push(ParsedContainer::Array(Vec::new()));
                    Ok(ValueStart::Nested)
                }
            }
            Some(b'{') => {
                self.begin_container(outer_depth, stack.len(), b'{')?;
                self.skip_whitespace();
                if self.peek() == Some(b'}') {
                    self.cursor += 1;
                    Ok(ValueStart::Complete(Value::Object(Map::new())))
                } else {
                    let key = self.parse_object_key()?;
                    stack.push(ParsedContainer::Object {
                        values: Map::new(),
                        key,
                    });
                    Ok(ValueStart::Nested)
                }
            }
            Some(b'-' | b'0'..=b'9') => {
                let range = self.parse_number_range()?;
                let text =
                    std::str::from_utf8(&self.bytes[range]).map_err(|_| EventError::InvalidData)?;
                let value = text.parse::<f64>().map_err(|_| EventError::InvalidData)?;
                Number::from_f64(value)
                    .map(Value::Number)
                    .map(ValueStart::Complete)
                    .ok_or(EventError::InvalidData)
            }
            _ => Err(EventError::GoJson("invalid value")),
        }
    }

    fn skip_value(&mut self, outer_depth: usize) -> Result<(), EventError> {
        let mut stack = Vec::new();
        loop {
            if matches!(
                self.skip_value_start(outer_depth, &mut stack)?,
                ValueStart::Nested
            ) {
                continue;
            }

            loop {
                let Some(container) = stack.last().copied() else {
                    return Ok(());
                };
                self.skip_whitespace();
                let container_complete = match (container, self.peek()) {
                    (SkippedContainer::Array, Some(b',')) => {
                        self.cursor += 1;
                        self.skip_whitespace();
                        false
                    }
                    (SkippedContainer::Array, Some(b']')) => {
                        self.cursor += 1;
                        true
                    }
                    (SkippedContainer::Object, Some(b',')) => {
                        self.cursor += 1;
                        self.skip_whitespace();
                        self.skip_object_key()?;
                        false
                    }
                    (SkippedContainer::Object, Some(b'}')) => {
                        self.cursor += 1;
                        true
                    }
                    (SkippedContainer::Array, _) => {
                        return Err(EventError::GoJson("expected array delimiter"));
                    }
                    (SkippedContainer::Object, _) => {
                        return Err(EventError::GoJson("expected object delimiter"));
                    }
                };
                if !container_complete {
                    break;
                }
                let _ = stack.pop();
            }
        }
    }

    fn skip_value_start(
        &mut self,
        outer_depth: usize,
        stack: &mut Vec<SkippedContainer>,
    ) -> Result<ValueStart<()>, EventError> {
        match self.peek() {
            Some(b'n') => self
                .parse_literal(b"null")
                .map(|()| ValueStart::Complete(())),
            Some(b't') => self
                .parse_literal(b"true")
                .map(|()| ValueStart::Complete(())),
            Some(b'f') => self
                .parse_literal(b"false")
                .map(|()| ValueStart::Complete(())),
            Some(b'"') => self.skip_string().map(|()| ValueStart::Complete(())),
            Some(b'[') => {
                self.begin_container(outer_depth, stack.len(), b'[')?;
                self.skip_whitespace();
                if self.peek() == Some(b']') {
                    self.cursor += 1;
                    Ok(ValueStart::Complete(()))
                } else {
                    stack.push(SkippedContainer::Array);
                    Ok(ValueStart::Nested)
                }
            }
            Some(b'{') => {
                self.begin_container(outer_depth, stack.len(), b'{')?;
                self.skip_whitespace();
                if self.peek() == Some(b'}') {
                    self.cursor += 1;
                    Ok(ValueStart::Complete(()))
                } else {
                    self.skip_object_key()?;
                    stack.push(SkippedContainer::Object);
                    Ok(ValueStart::Nested)
                }
            }
            Some(b'-' | b'0'..=b'9') => self.parse_number_range().map(|_| ValueStart::Complete(())),
            _ => Err(EventError::GoJson("invalid value")),
        }
    }

    fn begin_container(
        &mut self,
        outer_depth: usize,
        stack_depth: usize,
        opener: u8,
    ) -> Result<(), EventError> {
        if outer_depth + stack_depth >= GO_JSON_MAX_NESTING_DEPTH {
            return Err(EventError::GoJson("exceeded max depth"));
        }
        self.expect(opener)
    }

    fn parse_object_key(&mut self) -> Result<String, EventError> {
        let key = self.parse_string()?;
        self.skip_whitespace();
        self.expect(b':')?;
        self.skip_whitespace();
        Ok(key)
    }

    fn skip_object_key(&mut self) -> Result<(), EventError> {
        self.skip_string()?;
        self.skip_whitespace();
        self.expect(b':')?;
        self.skip_whitespace();
        Ok(())
    }

    fn parse_number_range(&mut self) -> Result<Range<usize>, EventError> {
        let start = self.cursor;
        if self.peek() == Some(b'-') {
            self.cursor += 1;
        }
        match self.peek() {
            Some(b'0') => self.cursor += 1,
            Some(b'1'..=b'9') => {
                self.cursor += 1;
                while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    self.cursor += 1;
                }
            }
            _ => return Err(EventError::GoJson("invalid number")),
        }
        if self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            return Err(EventError::GoJson("leading zero in number"));
        }
        if self.peek() == Some(b'.') {
            self.cursor += 1;
            let fraction_start = self.cursor;
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.cursor += 1;
            }
            if self.cursor == fraction_start {
                return Err(EventError::GoJson("empty number fraction"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.cursor += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.cursor += 1;
            }
            let exponent_start = self.cursor;
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.cursor += 1;
            }
            if self.cursor == exponent_start {
                return Err(EventError::GoJson("empty number exponent"));
            }
        }
        Ok(start..self.cursor)
    }
}

fn decode_go_json_string(raw: &[u8]) -> Result<String, EventError> {
    if raw.len() < 2 || raw.first() != Some(&b'"') || raw.last() != Some(&b'"') {
        return Err(EventError::GoJson("invalid quoted string"));
    }
    let mut output = String::new();
    let mut cursor = 1usize;
    let end = raw.len() - 1;
    let mut chunk_start = cursor;
    while cursor < end {
        if raw[cursor] != b'\\' {
            cursor += 1;
            continue;
        }
        append_go_utf8_replacement(&mut output, &raw[chunk_start..cursor]);
        cursor += 1;
        let escape = raw[cursor];
        cursor += 1;
        match escape {
            b'"' => output.push('"'),
            b'\\' => output.push('\\'),
            b'/' => output.push('/'),
            b'b' => output.push('\u{0008}'),
            b'f' => output.push('\u{000c}'),
            b'n' => output.push('\n'),
            b'r' => output.push('\r'),
            b't' => output.push('\t'),
            b'u' => {
                let first = parse_hex_u16(&raw[cursor..cursor + 4]);
                cursor += 4;
                if (0xd800..=0xdbff).contains(&first) && raw.get(cursor..cursor + 2) == Some(b"\\u")
                {
                    let second = parse_hex_u16(&raw[cursor + 2..cursor + 6]);
                    if (0xdc00..=0xdfff).contains(&second) {
                        cursor += 6;
                        let scalar = 0x1_0000
                            + ((u32::from(first) - 0xd800) << 10)
                            + (u32::from(second) - 0xdc00);
                        if let Some(character) = char::from_u32(scalar) {
                            output.push(character);
                        }
                    } else {
                        output.push('\u{fffd}');
                    }
                } else if (0xd800..=0xdfff).contains(&first) {
                    output.push('\u{fffd}');
                } else if let Some(character) = char::from_u32(u32::from(first)) {
                    output.push(character);
                }
            }
            _ => return Err(EventError::GoJson("invalid string escape")),
        }
        chunk_start = cursor;
    }
    append_go_utf8_replacement(&mut output, &raw[chunk_start..end]);
    Ok(output)
}

fn append_go_utf8_replacement(output: &mut String, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(valid) => {
                output.push_str(valid);
                return;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid > 0 {
                    // The prefix is certified by `Utf8Error::valid_up_to`.
                    if let Ok(prefix) = std::str::from_utf8(&bytes[..valid]) {
                        output.push_str(prefix);
                    }
                }
                output.push('\u{fffd}');
                bytes = &bytes[valid + 1..];
            }
        }
    }
}

fn parse_hex_u16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0u16, |value, byte| {
        let digit = match byte {
            b'0'..=b'9' => u16::from(*byte - b'0'),
            b'a'..=b'f' => u16::from(*byte - b'a' + 10),
            b'A'..=b'F' => u16::from(*byte - b'A' + 10),
            _ => 0,
        };
        value * 16 + digit
    })
}

/// Normalizes the raw, escape-free bytes of an RFC3339 timestamp the way Go's
/// `time.Time.UnmarshalJSON` materializes its `time.Time` value.
///
/// The returned spelling matches `time.Time.Format(time.RFC3339Nano)`,
/// including Go's accepted fallback spellings (comma fractions, a one-digit
/// hour, excessive fractional precision, and zone minute 60). Callers parsing
/// JSON directly must reject JSON escapes before passing the bytes: Go's
/// `time.Time.UnmarshalJSON` parses the raw bytes between quotes rather than
/// decoding string escapes first.
#[must_use]
pub fn normalize_go_time_unmarshal_rfc3339(value: &[u8]) -> Option<String> {
    if !value.is_ascii()
        || value.len() < 19
        || value.get(4) != Some(&b'-')
        || value.get(7) != Some(&b'-')
        || value.get(10) != Some(&b'T')
    {
        return None;
    }

    let year = ascii_decimal(value.get(0..4))?;
    let month = ascii_decimal(value.get(5..7))?;
    let day = ascii_decimal(value.get(8..10))?;
    let leap_year = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap_year => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => 0,
    };
    if day == 0 || day > days_in_month {
        return None;
    }

    let hour_colon_offset = value[11..].iter().position(|byte| *byte == b':')?;
    let hour_end = 11 + hour_colon_offset;
    if !(hour_end == 12 || hour_end == 13)
        || ascii_decimal(value.get(11..hour_end)).is_none_or(|hour| hour > 23)
    {
        return None;
    }
    let minute_start = hour_end + 1;
    let second_start = minute_start + 3;
    if value.get(minute_start + 2) != Some(&b':')
        || ascii_decimal(value.get(minute_start..minute_start + 2)).is_none_or(|minute| minute > 59)
        || ascii_decimal(value.get(second_start..second_start + 2)).is_none_or(|second| second > 59)
    {
        return None;
    }

    let mut cursor = second_start + 2;
    if matches!(value.get(cursor), Some(b'.' | b',')) {
        cursor += 1;
        let fraction_start = cursor;
        while value.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor == fraction_start {
            return None;
        }
    }

    let zone = match value.get(cursor) {
        Some(b'Z') if cursor + 1 == value.len() => "Z".to_owned(),
        Some(b'+' | b'-') => {
            if cursor + 6 != value.len() || value.get(cursor + 3) != Some(&b':') {
                return None;
            }
            let hour = ascii_decimal(value.get(cursor + 1..cursor + 3))?;
            let minute = ascii_decimal(value.get(cursor + 4..cursor + 6))?;
            if hour > 24 || minute > 60 {
                return None;
            }
            let total_minutes = hour * 60 + minute;
            if total_minutes == 0 {
                "Z".to_owned()
            } else {
                format!(
                    "{}{zone_hour:02}:{zone_minute:02}",
                    char::from(value[cursor]),
                    zone_hour = total_minutes / 60,
                    zone_minute = total_minutes % 60,
                )
            }
        }
        _ => return None,
    };

    let fraction_end = cursor;
    let fraction_start = second_start + 2;
    let fraction = if matches!(value.get(fraction_start), Some(b'.' | b',')) {
        let digits = &value[fraction_start + 1..fraction_end];
        let retained = &digits[..digits.len().min(9)];
        let trimmed = retained
            .iter()
            .rposition(|digit| *digit != b'0')
            .map_or(&[][..], |last| &retained[..=last]);
        if trimmed.is_empty() {
            String::new()
        } else {
            format!(".{}", std::str::from_utf8(trimmed).ok()?)
        }
    } else {
        String::new()
    };

    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{fraction}{zone}",
        hour = ascii_decimal(value.get(11..hour_end))?,
        minute = ascii_decimal(value.get(minute_start..minute_start + 2))?,
        second = ascii_decimal(value.get(second_start..second_start + 2))?,
    ))
}

fn ascii_decimal(bytes: Option<&[u8]>) -> Option<u32> {
    let bytes = bytes?;
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    bytes.iter().try_fold(0u32, |value, byte| {
        value.checked_mul(10)?.checked_add(u32::from(*byte - b'0'))
    })
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn validate_event_record(value: &EventRecord) -> Result<(), EventError> {
    for (field, value) in [
        ("id", value.id.as_str()),
        ("type", value.event_type.as_str()),
        ("mission_id", value.mission_id.as_str()),
    ] {
        if value.is_empty() {
            return Err(EventError::InvalidField {
                field,
                expected: "non-empty string",
            });
        }
    }
    if !is_rfc3339(&value.timestamp) {
        return Err(EventError::InvalidTimestamp(value.timestamp.clone()));
    }
    Ok(())
}

// Strictly checks the RFC3339 grammar and numeric ranges used by Go's time parser.
fn is_rfc3339(value: &str) -> bool {
    if value.len() < 20 || !value.is_ascii() {
        return false;
    }
    let bytes = value.as_bytes();
    if bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return false;
    }
    let number = |range: std::ops::Range<usize>| -> Option<u32> {
        std::str::from_utf8(bytes.get(range)?).ok()?.parse().ok()
    };
    let (Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        number(5..7),
        number(8..10),
        number(11..13),
        number(14..16),
        number(17..19),
    ) else {
        return false;
    };
    let year = number(0..4).unwrap_or(0);
    let leap_year = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap_year => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => 0,
    };
    if day == 0 || day > days_in_month || hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    let mut offset = 19;
    if bytes.get(offset) == Some(&b'.') {
        offset += 1;
        let fraction_start = offset;
        while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            offset += 1;
        }
        if offset == fraction_start {
            return false;
        }
    }
    match bytes.get(offset) {
        Some(b'Z') => offset + 1 == bytes.len(),
        Some(b'+') | Some(b'-') => {
            if offset + 6 != bytes.len() || bytes.get(offset + 3) != Some(&b':') {
                return false;
            }
            let zone_hour = std::str::from_utf8(&bytes[offset + 1..offset + 3])
                .ok()
                .and_then(|value| value.parse::<u32>().ok());
            let zone_minute = std::str::from_utf8(&bytes[offset + 4..offset + 6])
                .ok()
                .and_then(|value| value.parse::<u32>().ok());
            matches!((zone_hour, zone_minute), (Some(0..=23), Some(0..=59)))
        }
        _ => false,
    }
}

struct CurrentEvent<'a>(&'a EventRecord);

impl serde::Serialize for CurrentEvent<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = self.0;
        let mut map = serializer.serialize_map(None)?;
        for (key, item) in value.extra.iter() {
            if !event_reserved(key) {
                map.serialize_entry(key, item)?;
            }
        }
        map.serialize_entry("id", &value.id)?;
        map.serialize_entry("type", &value.event_type)?;
        map.serialize_entry("timestamp", &value.timestamp)?;
        map.serialize_entry("sequence", &value.sequence)?;
        map.serialize_entry("mission_id", &value.mission_id)?;
        if let Some(phase_id) = &value.phase_id {
            if !phase_id.is_empty() {
                map.serialize_entry("phase_id", phase_id)?;
            }
        }
        if let Some(worker_id) = &value.worker_id {
            if !worker_id.is_empty() {
                map.serialize_entry("worker_id", worker_id)?;
            }
        }
        if let Some(data) = &value.data {
            if !data.is_empty() {
                map.serialize_entry("data", &data.values)?;
            }
        }
        map.end()
    }
}

fn event_reserved(key: &str) -> bool {
    matches!(
        key,
        "id" | "type" | "timestamp" | "sequence" | "mission_id" | "phase_id" | "worker_id" | "data"
    )
}

#[cfg(test)]
mod tests {
    use super::{
        CheckpointPhase, CheckpointPlan, CheckpointProjection, decode_checkpoint,
        encode_current_checkpoint, encode_current_plan,
    };
    use serde_json::json;

    fn sample_plan() -> CheckpointPlan {
        CheckpointPlan {
            id: "plan-7".to_owned(),
            task: "port codec".to_owned(),
            phases: vec![CheckpointPhase {
                id: "phase-1".to_owned(),
                name: "implement".to_owned(),
                objective: "encode plan.json".to_owned(),
                persona: "implementer".to_owned(),
                model_tier: "work".to_owned(),
                skills: vec!["golang-cli".to_owned()],
                constraints: vec!["no network".to_owned()],
                dependencies: Vec::new(),
                expected: "byte-exact plan.json".to_owned(),
                status: "pending".to_owned(),
                ..CheckpointPhase::default()
            }],
            execution_mode: "sequential".to_owned(),
            decomp_source: "predecomposed".to_owned(),
            created_at: "2026-07-17T00:00:00Z".to_owned(),
            ..CheckpointPlan::default()
        }
    }

    /// Hand-derived from the frozen Go snapshot's
    /// `json.MarshalIndent(plan, "", "  ")` at
    /// `skills/orchestrator/internal/cmd/run.go:444`, matching `core.Plan`
    /// and `core.Phase`'s declared field order
    /// (`internal/core/types.go:9-16,33-42,71`) with no `omitempty` on any
    /// of these fields: two-space indent, no trailing newline. This fixture
    /// carries none of the `omitempty` policy/runtime-state fields
    /// (`role` through `barok_first_run_session_id`,
    /// `internal/core/types.go:46-129`), so every one of them is Go's zero
    /// value and correctly omitted here too.
    const EXPECTED_PLAN_JSON: &str = "{\n  \"id\": \"plan-7\",\n  \"task\": \"port codec\",\n  \"phases\": [\n    {\n      \"id\": \"phase-1\",\n      \"name\": \"implement\",\n      \"objective\": \"encode plan.json\",\n      \"persona\": \"implementer\",\n      \"model_tier\": \"work\",\n      \"skills\": [\n        \"golang-cli\"\n      ],\n      \"constraints\": [\n        \"no network\"\n      ],\n      \"dependencies\": [],\n      \"expected\": \"byte-exact plan.json\",\n      \"status\": \"pending\"\n    }\n  ],\n  \"execution_mode\": \"sequential\",\n  \"decomp_source\": \"predecomposed\",\n  \"created_at\": \"2026-07-17T00:00:00Z\"\n}";

    #[test]
    fn encode_current_plan_matches_frozen_go_byte_format() -> Result<(), Box<dyn std::error::Error>>
    {
        let bytes = encode_current_plan(&sample_plan())?;
        assert_eq!(String::from_utf8(bytes)?, EXPECTED_PLAN_JSON);
        Ok(())
    }

    /// A realistic decomposed phase the way `applyRuntimePolicy`,
    /// `annotateRoles`, and pre-decomposed `WORKDIR:`/`PRIORITY:` PHASE
    /// lines populate it (`internal/decompose/decompose.go:897-909,
    /// 1237-1245,1257-1266`): `role`, `target_dir`, `priority`, `runtime`,
    /// `runtime_policy_applied`, and `stall_timeout` are all set alongside
    /// the ten fields the writer already modeled. Before Fix 1 these six
    /// fields lived in `extra` and were emitted *before* the reserved
    /// fields; this fixture is the regression guard for that bug.
    fn sample_plan_with_policy_fields() -> CheckpointPlan {
        let mut plan = sample_plan();
        plan.phases[0].role = "implementer".to_owned();
        plan.phases[0].target_dir = "skills/orchestrator-rs".to_owned();
        plan.phases[0].priority = "P0".to_owned();
        plan.phases[0].runtime = "claude".to_owned();
        plan.phases[0].runtime_policy_applied = true;
        plan.phases[0].stall_timeout = 1_800_000_000_000; // 30m in Go time.Duration ns
        plan
    }

    /// Hand-derived the same way as `EXPECTED_PLAN_JSON`, inserting the six
    /// `omitempty` policy/runtime-state fields between `expected` and
    /// `status` — their exact declared position
    /// (`internal/core/types.go:43-68`, immediately after `Expected` and
    /// immediately before the `Status` runtime-state block starting at
    /// `types.go:71`). `stall_timeout` (`types.go:68`) is a plain
    /// `time.Duration` (int64 nanoseconds) with no custom
    /// `MarshalJSON`, so Go emits the bare integer `1800000000000`.
    const EXPECTED_PLAN_JSON_WITH_POLICY_FIELDS: &str = "{\n  \"id\": \"plan-7\",\n  \"task\": \"port codec\",\n  \"phases\": [\n    {\n      \"id\": \"phase-1\",\n      \"name\": \"implement\",\n      \"objective\": \"encode plan.json\",\n      \"persona\": \"implementer\",\n      \"model_tier\": \"work\",\n      \"skills\": [\n        \"golang-cli\"\n      ],\n      \"constraints\": [\n        \"no network\"\n      ],\n      \"dependencies\": [],\n      \"expected\": \"byte-exact plan.json\",\n      \"role\": \"implementer\",\n      \"target_dir\": \"skills/orchestrator-rs\",\n      \"priority\": \"P0\",\n      \"runtime\": \"claude\",\n      \"runtime_policy_applied\": true,\n      \"stall_timeout\": 1800000000000,\n      \"status\": \"pending\"\n    }\n  ],\n  \"execution_mode\": \"sequential\",\n  \"decomp_source\": \"predecomposed\",\n  \"created_at\": \"2026-07-17T00:00:00Z\"\n}";

    #[test]
    fn encode_current_plan_matches_frozen_go_byte_format_with_policy_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let bytes = encode_current_plan(&sample_plan_with_policy_fields())?;
        assert_eq!(
            String::from_utf8(bytes)?,
            EXPECTED_PLAN_JSON_WITH_POLICY_FIELDS
        );
        Ok(())
    }

    /// Regression guard for the pre-Fix-1 bug: policy/runtime fields must
    /// never appear before the reserved fields they belong among (that
    /// only happens to genuinely unknown `extra` keys).
    #[test]
    fn encode_current_plan_orders_policy_fields_after_expected_not_before_id()
    -> Result<(), Box<dyn std::error::Error>> {
        let output = String::from_utf8(encode_current_plan(&sample_plan_with_policy_fields())?)?;
        let id_pos = output.find("\"id\": \"phase-1\"").ok_or("missing id")?;
        let runtime_pos = output
            .find("\"runtime\": \"claude\"")
            .ok_or("missing runtime")?;
        assert!(
            id_pos < runtime_pos,
            "expected reserved field `id` before policy field `runtime`"
        );
        Ok(())
    }

    /// A JSON checkpoint fixture (envelope-v1/payload-v2) carrying every
    /// one of the 38 newly-typed phase fields
    /// (`internal/core/types.go:46-129`) with a non-zero/non-empty value,
    /// so decode extraction and the `CurrentPhase` serializer are both
    /// exercised for the complete field set in one round trip.
    fn full_phase_checkpoint_bytes() -> Vec<u8> {
        br#"{
  "version": 1,
  "payload": {
    "version": 2,
    "workspace_id": "20260717-full-phase",
    "domain": "dev",
    "plan": {
      "id": "plan-full",
      "task": "exercise every typed field",
      "phases": [
        {
          "id": "phase-1",
          "name": "implement",
          "objective": "exercise fields",
          "persona": "implementer",
          "model_tier": "work",
          "skills": ["golang-cli"],
          "constraints": ["no network"],
          "dependencies": ["phase-0"],
          "expected": "byte-exact plan.json",
          "role": "implementer",
          "target_dir": "skills/orchestrator-rs",
          "priority": "P0",
          "runtime": "claude",
          "runtime_policy_applied": true,
          "stall_timeout": 1800000000000,
          "status": "completed",
          "output": "did the thing",
          "error": "transient hiccup",
          "start_time": "2026-07-17T00:00:00Z",
          "end_time": "2026-07-17T00:05:00Z",
          "signal_remainder": "finish X next",
          "retries": 2,
          "gate_passed": true,
          "output_len": 4096,
          "parsed_skills": ["golang-cli", "golang-testing"],
          "learnings_retrieved": 3,
          "session_id": "sess-123",
          "persona_selection_method": "llm",
          "model": "claude-sonnet-5",
          "tokens_in": 1000,
          "tokens_out": 500,
          "tokens_cache_creation": 200,
          "tokens_cache_read": 100,
          "cost_usd": 1.25,
          "review_iteration": 1,
          "origin_phase_id": "phase-0",
          "max_review_loops": 2,
          "parse_retry_count": 1,
          "review_blockers": ["missing test"],
          "review_warnings": ["style nit"],
          "changed_files": ["src/codec.rs"],
          "output_bytes": 8192,
          "worker": "alpha",
          "barok_applied": 1,
          "barok_retry": 1,
          "barok_validator_ms": 12,
          "barok_retry_validator_ms": 7,
          "barok_first_run_session_id": "sess-000"
        }
      ],
      "execution_mode": "sequential",
      "decomp_source": "predecomposed",
      "created_at": "2026-07-17T00:00:00Z"
    },
    "status": "in_progress",
    "started_at": "2026-07-17T00:00:00Z"
  }
}"#
        .to_vec()
    }

    #[test]
    fn decode_populates_every_typed_phase_field_instead_of_extra()
    -> Result<(), Box<dyn std::error::Error>> {
        let decoded = decode_checkpoint(&full_phase_checkpoint_bytes())?;
        let plan = decoded.projection.plan.as_ref().ok_or("missing plan")?;
        let phase = plan.phases.first().ok_or("missing phase")?;

        assert_eq!(phase.role, "implementer");
        assert_eq!(phase.target_dir, "skills/orchestrator-rs");
        assert_eq!(phase.priority, "P0");
        assert_eq!(phase.runtime, "claude");
        assert!(phase.runtime_policy_applied);
        assert_eq!(phase.stall_timeout, 1_800_000_000_000);
        assert_eq!(phase.output, "did the thing");
        assert_eq!(phase.error, "transient hiccup");
        assert_eq!(phase.start_time, "2026-07-17T00:00:00Z");
        assert_eq!(phase.end_time, "2026-07-17T00:05:00Z");
        assert_eq!(phase.signal_remainder, "finish X next");
        assert_eq!(phase.retries, 2);
        assert!(phase.gate_passed);
        assert_eq!(phase.output_len, 4096);
        assert_eq!(
            phase.parsed_skills,
            vec!["golang-cli".to_owned(), "golang-testing".to_owned()]
        );
        assert_eq!(phase.learnings_retrieved, 3);
        assert_eq!(phase.session_id, "sess-123");
        assert_eq!(phase.persona_selection_method, "llm");
        assert_eq!(phase.model, "claude-sonnet-5");
        assert_eq!(phase.tokens_in, 1000);
        assert_eq!(phase.tokens_out, 500);
        assert_eq!(phase.tokens_cache_creation, 200);
        assert_eq!(phase.tokens_cache_read, 100);
        assert!((phase.cost_usd - 1.25).abs() < f64::EPSILON);
        assert_eq!(phase.review_iteration, 1);
        assert_eq!(phase.origin_phase_id, "phase-0");
        assert_eq!(phase.max_review_loops, 2);
        assert_eq!(phase.parse_retry_count, 1);
        assert_eq!(phase.review_blockers, vec!["missing test".to_owned()]);
        assert_eq!(phase.review_warnings, vec!["style nit".to_owned()]);
        assert_eq!(phase.changed_files, vec!["src/codec.rs".to_owned()]);
        assert_eq!(phase.output_bytes, 8192);
        assert_eq!(phase.worker, "alpha");
        assert_eq!(phase.barok_applied, 1);
        assert_eq!(phase.barok_retry, 1);
        assert_eq!(phase.barok_validator_ms, 12);
        assert_eq!(phase.barok_retry_validator_ms, 7);
        assert_eq!(phase.barok_first_run_session_id, "sess-000");

        // None of these 38 fields should have landed in `extra`: they are
        // all reserved now, so `extra` should be empty for this fixture.
        assert!(
            phase.extra.is_empty(),
            "unexpected leftover extra keys: {:?}",
            phase.extra
        );
        Ok(())
    }

    #[test]
    fn decode_then_encode_round_trips_every_typed_phase_field()
    -> Result<(), Box<dyn std::error::Error>> {
        let decoded = decode_checkpoint(&full_phase_checkpoint_bytes())?;
        let bytes = encode_current_checkpoint(&decoded.projection)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let phase = &value["payload"]["plan"]["phases"][0];

        assert_eq!(phase["role"], "implementer");
        assert_eq!(phase["target_dir"], "skills/orchestrator-rs");
        assert_eq!(phase["priority"], "P0");
        assert_eq!(phase["runtime"], "claude");
        assert_eq!(phase["runtime_policy_applied"], true);
        assert_eq!(phase["stall_timeout"], 1_800_000_000_000i64);
        assert_eq!(phase["output"], "did the thing");
        assert_eq!(phase["error"], "transient hiccup");
        assert_eq!(phase["start_time"], "2026-07-17T00:00:00Z");
        assert_eq!(phase["end_time"], "2026-07-17T00:05:00Z");
        assert_eq!(phase["signal_remainder"], "finish X next");
        assert_eq!(phase["retries"], 2);
        assert_eq!(phase["gate_passed"], true);
        assert_eq!(phase["output_len"], 4096);
        assert_eq!(
            phase["parsed_skills"],
            json!(["golang-cli", "golang-testing"])
        );
        assert_eq!(phase["learnings_retrieved"], 3);
        assert_eq!(phase["session_id"], "sess-123");
        assert_eq!(phase["persona_selection_method"], "llm");
        assert_eq!(phase["model"], "claude-sonnet-5");
        assert_eq!(phase["tokens_in"], 1000);
        assert_eq!(phase["tokens_out"], 500);
        assert_eq!(phase["tokens_cache_creation"], 200);
        assert_eq!(phase["tokens_cache_read"], 100);
        assert_eq!(phase["cost_usd"], 1.25);
        assert_eq!(phase["review_iteration"], 1);
        assert_eq!(phase["origin_phase_id"], "phase-0");
        assert_eq!(phase["max_review_loops"], 2);
        assert_eq!(phase["parse_retry_count"], 1);
        assert_eq!(phase["review_blockers"], json!(["missing test"]));
        assert_eq!(phase["review_warnings"], json!(["style nit"]));
        assert_eq!(phase["changed_files"], json!(["src/codec.rs"]));
        assert_eq!(phase["output_bytes"], 8192);
        assert_eq!(phase["worker"], "alpha");
        assert_eq!(phase["barok_applied"], 1);
        assert_eq!(phase["barok_retry"], 1);
        assert_eq!(phase["barok_validator_ms"], 12);
        assert_eq!(phase["barok_retry_validator_ms"], 7);
        assert_eq!(phase["barok_first_run_session_id"], "sess-000");
        Ok(())
    }

    /// Every `omitempty` field at its Go zero value must be absent from the
    /// output entirely (never emitted as `""`/`0`/`false`/`[]`).
    #[test]
    fn encode_current_plan_omits_every_zero_value_policy_field()
    -> Result<(), Box<dyn std::error::Error>> {
        let output = String::from_utf8(encode_current_plan(&sample_plan())?)?;
        for key in [
            "\"role\"",
            "\"target_dir\"",
            "\"priority\"",
            "\"runtime\"",
            "\"runtime_policy_applied\"",
            "\"stall_timeout\"",
            "\"output\"",
            "\"error\"",
            "\"start_time\"",
            "\"end_time\"",
            "\"signal_remainder\"",
            "\"retries\"",
            "\"gate_passed\"",
            "\"output_len\"",
            "\"parsed_skills\"",
            "\"learnings_retrieved\"",
            "\"session_id\"",
            "\"persona_selection_method\"",
            "\"model\"",
            "\"tokens_in\"",
            "\"tokens_out\"",
            "\"tokens_cache_creation\"",
            "\"tokens_cache_read\"",
            "\"cost_usd\"",
            "\"review_iteration\"",
            "\"origin_phase_id\"",
            "\"max_review_loops\"",
            "\"parse_retry_count\"",
            "\"review_blockers\"",
            "\"review_warnings\"",
            "\"changed_files\"",
            "\"output_bytes\"",
            "\"worker\"",
            "\"barok_applied\"",
            "\"barok_retry\"",
            "\"barok_validator_ms\"",
            "\"barok_retry_validator_ms\"",
            "\"barok_first_run_session_id\"",
        ] {
            assert!(!output.contains(key), "unexpected key {key} in {output}");
        }
        Ok(())
    }

    #[test]
    fn encode_current_plan_rejects_invalid_phase_start_time() {
        let mut plan = sample_plan();
        plan.phases[0].start_time = "not-a-time".to_owned();
        assert!(encode_current_plan(&plan).is_err());
    }

    #[test]
    fn encode_current_plan_rejects_invalid_phase_end_time() {
        let mut plan = sample_plan();
        plan.phases[0].end_time = "not-a-time".to_owned();
        assert!(encode_current_plan(&plan).is_err());
    }

    #[test]
    fn encode_current_plan_defaults_empty_created_at_to_go_zero_time()
    -> Result<(), Box<dyn std::error::Error>> {
        let plan = CheckpointPlan::default();
        let bytes = encode_current_plan(&plan)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(value["created_at"], "0001-01-01T00:00:00Z");
        assert_eq!(value["phases"], json!([]));
        assert!(!String::from_utf8(bytes)?.ends_with('\n'));
        Ok(())
    }

    #[test]
    fn encode_current_plan_preserves_unknown_plan_and_phase_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut plan = sample_plan();
        plan.extra
            .insert("plan_future".to_owned(), json!({"nested": "value"}));
        plan.phases[0]
            .extra
            .insert("phase_future".to_owned(), json!([1, 2, 3]));
        let bytes = encode_current_plan(&plan)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(value["plan_future"]["nested"], "value");
        assert_eq!(value["phases"][0]["phase_future"], json!([1, 2, 3]));

        // Unknown fields are emitted ahead of the reserved fields, matching
        // the checkpoint payload's established CurrentPlan/CurrentPhase
        // convention (see checkpoint_current_writer_has_go_field_order_and_versions
        // in tests/compatibility_codecs.rs).
        let output = String::from_utf8(bytes)?;
        assert!(
            output
                .starts_with("{\n  \"plan_future\": {\n    \"nested\": \"value\"\n  },\n  \"id\":")
        );
        Ok(())
    }

    #[test]
    fn encode_current_plan_filters_reserved_keys_out_of_extra()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut plan = sample_plan();
        plan.extra.insert("task".to_owned(), json!("shadow-task"));
        plan.phases[0]
            .extra
            .insert("status".to_owned(), json!("shadow-status"));
        let bytes = encode_current_plan(&plan)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(value["task"], "port codec");
        assert_eq!(value["phases"][0]["status"], "pending");
        Ok(())
    }

    #[test]
    fn encode_current_plan_rejects_invalid_created_at() {
        let mut plan = sample_plan();
        plan.created_at = "not-a-time".to_owned();
        assert!(encode_current_plan(&plan).is_err());
    }

    #[test]
    fn encode_current_plan_rejects_go_unreadable_phase_extra_fields() {
        let mut plan = sample_plan();
        plan.phases[0]
            .extra
            .insert("retries".to_owned(), json!("many"));
        assert!(encode_current_plan(&plan).is_err());
    }

    /// `encode_current_plan` reuses the same `CurrentPlan` serializer that
    /// `encode_current_checkpoint` embeds at `payload.plan`, so the two
    /// vocabularies cannot drift: encoding a plan standalone must produce
    /// the identical JSON structure that the same plan produces nested
    /// inside a checkpoint.
    #[test]
    fn encode_current_plan_matches_the_plan_embedded_in_a_checkpoint()
    -> Result<(), Box<dyn std::error::Error>> {
        let plan = sample_plan();
        let standalone: serde_json::Value = serde_json::from_slice(&encode_current_plan(&plan)?)?;

        let projection = CheckpointProjection {
            workspace_id: "20260717-drift-check".to_owned(),
            plan: Some(plan),
            ..CheckpointProjection::default()
        };
        let checkpoint_bytes = super::encode_current_checkpoint(&projection)?;
        let checkpoint: serde_json::Value = serde_json::from_slice(&checkpoint_bytes)?;

        assert_eq!(standalone, checkpoint["payload"]["plan"]);
        Ok(())
    }

    /// Go's `encoding/json` `Marshal`/`MarshalIndent` HTML-escape `<`, `>`,
    /// and `&` to `<`/`>`/`&` unconditionally (no
    /// `Encoder.SetEscapeHTML(false)` exists anywhere in the frozen Go
    /// snapshot — confirmed by grep), so `plan.json`'s
    /// `json.MarshalIndent(plan, "", "  ")` at
    /// `internal/cmd/run.go:444` does the same. `encode_current_plan` must
    /// match byte-for-byte.
    #[test]
    fn encode_current_plan_escapes_html_significant_characters_like_go()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut plan = sample_plan();
        plan.task = "a && b < c > d".to_owned();
        let output = String::from_utf8(encode_current_plan(&plan)?)?;
        assert!(output.contains(r#""task": "a \u0026\u0026 b \u003c c \u003e d""#));
        assert!(!output.contains('<'));
        assert!(!output.contains('>'));
        assert!(!output.contains('&'));

        // The escaped bytes still round-trip to the original characters
        // through a normal JSON parser.
        let value: serde_json::Value = serde_json::from_str(&output)?;
        assert_eq!(value["task"], "a && b < c > d");
        Ok(())
    }

    /// Same contract as the plan test, applied to the checkpoint payload's
    /// embedded plan (`saveCheckpointInternal`'s
    /// `json.MarshalIndent(envelope, "", "  ")` at
    /// `internal/core/checkpoint.go:77`).
    #[test]
    fn encode_current_checkpoint_escapes_html_significant_characters_like_go()
    -> Result<(), Box<dyn std::error::Error>> {
        let projection = CheckpointProjection {
            workspace_id: "20260717-html-escape".to_owned(),
            plan: Some(CheckpointPlan {
                task: "a && b < c > d".to_owned(),
                ..CheckpointPlan::default()
            }),
            ..CheckpointProjection::default()
        };
        let output = String::from_utf8(encode_current_checkpoint(&projection)?)?;
        assert!(output.contains(r#""task": "a \u0026\u0026 b \u003c c \u003e d""#));
        assert!(!output.contains('<'));
        assert!(!output.contains('>'));
        assert!(!output.contains('&'));
        Ok(())
    }

    /// Go additionally escapes U+2028 (LINE SEPARATOR) and U+2029
    /// (PARAGRAPH SEPARATOR) to ` `/` `. Both are 3-byte UTF-8
    /// sequences, so this exercises multi-byte-character escaping rather
    /// than the single-byte ASCII cases above.
    #[test]
    fn encode_current_plan_escapes_unicode_line_and_paragraph_separators()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut plan = sample_plan();
        plan.task = "line\u{2028}sep and para\u{2029}sep".to_owned();
        let output = String::from_utf8(encode_current_plan(&plan)?)?;
        assert!(output.contains(r"line\u2028sep and para\u2029sep"));
        assert!(!output.contains('\u{2028}'));
        assert!(!output.contains('\u{2029}'));

        let value: serde_json::Value = serde_json::from_str(&output)?;
        assert_eq!(value["task"], "line\u{2028}sep and para\u{2029}sep");
        Ok(())
    }
}
