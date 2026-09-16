//! Read-only adapter for the Go orchestrator's audit scorecard surface
//! (`internal/audit/{types,store,scorecard}.go`).
//!
//! `LoadReports`/`BuildScorecard`/`FormatScorecard`/`FormatScorecardJSON`
//! are pure or local-file-read (`audits.jsonl`), so this module has **no
//! write path**: `SaveReport` (`store.go:25-50`) is not ported — appending
//! to `audits.jsonl` is a mutation and out of scope for a read-only
//! adapter, per `PORTING.md` §0.
//!
//! Ported (Go source, `internal/audit/*.go`):
//! - `LoadReports` → [`load_reports`] (malformed-JSONL-line tolerant, per
//!   `store.go:72-77`'s `continue // skip malformed lines`)
//! - `BuildScorecard`, `computeTrend`, `classifyTrend`, `extractScore`,
//!   `detectRegressions` → [`build_scorecard`] and its helpers
//! - `FormatScorecard`, `sparkline` → [`format_scorecard`]
//! - `FormatScorecardJSON` → [`format_scorecard_json`]
//!
//! Timestamps (`AuditedAt`) are validated and normalized on read with
//! `orchestrator-core`'s Go `time.Time.UnmarshalJSON` compatibility helper.
//! They are retained as canonical RFC3339Nano strings internally; regression
//! JSON drops fractional seconds to match `time.Time.Format(time.RFC3339)`.

use std::io::{BufRead, BufReader};
use std::path::Path;

use orchestrator_core::{
    EventError, GO_EVENT_JSON_CONTENT_MAX_BYTES, GO_ZERO_TIME, GoJsonScalar,
    decode_go_json_array_elements, decode_go_json_object_members, decode_go_json_scalar,
    go_json_field_matches, normalize_go_time_unmarshal_rfc3339,
};
use serde::{Serialize, Serializer};
use thiserror::Error;

use crate::fs_util;
use crate::runtime_home::{self, ReadOnlyEventLogTarget};

/// Failures raised while reading `audits.jsonl` or serializing a scorecard.
#[derive(Debug, Error)]
pub enum AuditReadError {
    /// Opening the audits file failed for a reason other than "does not
    /// exist" (Go: `store.go:60-66` treats `os.IsNotExist` as `nil, nil`).
    #[error("opening audits file: {source}")]
    Open {
        #[source]
        source: std::io::Error,
    },
    /// A line-read failed at the I/O layer (not a JSON parse failure —
    /// those are silently skipped, matching Go).
    #[error("reading audits file: {source}")]
    Read {
        #[source]
        source: std::io::Error,
    },
    /// `FormatScorecardJSON`'s `json.MarshalIndent` equivalent failed.
    #[error("marshaling scorecard: {source}")]
    Marshal {
        #[source]
        source: serde_json::Error,
    },
}

/// Mirrors Go's `Scorecard` (`types.go:25-32`).
#[derive(Debug, Clone, Default)]
pub struct Scorecard {
    pub decomposition_quality: i64,
    pub persona_fit: i64,
    pub skill_utilization: i64,
    pub output_quality: i64,
    pub rule_compliance: i64,
    pub overall: i64,
}

/// Mirrors Go's `Recommendation` (`types.go:55-60`).
#[derive(Debug, Clone, Default)]
pub struct Recommendation {
    pub category: String,
    pub priority: String,
    pub summary: String,
    pub detail: String,
}

/// Mirrors Go's `MissionEvaluation` (`types.go:35-40`).
#[derive(Debug, Clone, Default)]
pub struct MissionEvaluation {
    pub summary: String,
    pub strengths: Vec<String>,
    pub weaknesses: Vec<String>,
    pub recommendations: Vec<Recommendation>,
}

/// Mirrors Go's `PhaseEvaluation` (`types.go:43-52`).
#[derive(Debug, Clone, Default)]
pub struct PhaseEvaluation {
    pub phase_id: String,
    pub phase_name: String,
    pub persona_assigned: String,
    pub persona_ideal: String,
    pub persona_correct: bool,
    pub objective_met: bool,
    pub issues: Vec<String>,
    pub score: i64,
}

/// Mirrors Go's `ChangeRecord` (`types.go:63-69`). `kind` mirrors Go's
/// `Type` field (`json:"type"`) — `type` is a Rust keyword.
#[derive(Debug, Clone, Default)]
pub struct ChangeRecord {
    pub phase_id: String,
    pub phase_name: String,
    pub kind: String,
    pub target: String,
    pub summary: String,
}

/// Mirrors Go's `ConvergenceStatus` (`types.go:72-78`).
#[derive(Debug, Clone, Default)]
pub struct ConvergenceStatus {
    pub converged: bool,
    pub drift_phases: Vec<String>,
    pub missing_phases: Vec<String>,
    pub redundant_work: Vec<String>,
    pub assessment: String,
}

/// Mirrors Go's `DecomposerConvergence` (`types.go:81-87`).
#[derive(Debug, Clone, Default)]
pub struct DecomposerConvergence {
    pub skill_md_hash: String,
    pub prompt_source: String,
    pub skill_md_path: String,
    pub rules_extracted: bool,
}

/// Mirrors Go's `AuditReport` (`types.go:6-22`).
#[derive(Debug, Clone)]
pub struct AuditReport {
    pub workspace_id: String,
    pub task: String,
    pub domain: String,
    pub status: String,
    /// Canonical RFC3339Nano string — see module doc comment.
    pub audited_at: String,
    pub linear_issue_id: String,
    pub mission_path: String,
    pub scorecard: Scorecard,
    pub evaluation: MissionEvaluation,
    pub phases: Vec<PhaseEvaluation>,
    pub convergence: ConvergenceStatus,
    pub decomposer_convergence: DecomposerConvergence,
    pub changes: Vec<ChangeRecord>,
}

impl Default for AuditReport {
    fn default() -> Self {
        Self {
            workspace_id: String::new(),
            task: String::new(),
            domain: String::new(),
            status: String::new(),
            audited_at: default_go_time(),
            linear_issue_id: String::new(),
            mission_path: String::new(),
            scorecard: Scorecard::default(),
            evaluation: MissionEvaluation::default(),
            phases: Vec::new(),
            convergence: ConvergenceStatus::default(),
            decomposer_convergence: DecomposerConvergence::default(),
            changes: Vec::new(),
        }
    }
}

fn default_go_time() -> String {
    GO_ZERO_TIME.to_owned()
}

fn decode_go_string(
    raw: &[u8],
    destination: &mut String,
    field: &'static str,
) -> Result<(), EventError> {
    match decode_go_json_scalar(raw)? {
        GoJsonScalar::Null => Ok(()),
        GoJsonScalar::String { decoded, .. } => {
            *destination = decoded;
            Ok(())
        }
        _ => Err(EventError::InvalidField {
            field,
            expected: "string or null",
        }),
    }
}

fn decode_go_bool(
    raw: &[u8],
    destination: &mut bool,
    field: &'static str,
) -> Result<(), EventError> {
    match decode_go_json_scalar(raw)? {
        GoJsonScalar::Null => Ok(()),
        GoJsonScalar::Bool(value) => {
            *destination = value;
            Ok(())
        }
        _ => Err(EventError::InvalidField {
            field,
            expected: "boolean or null",
        }),
    }
}

fn decode_go_i64(raw: &[u8], destination: &mut i64, field: &'static str) -> Result<(), EventError> {
    match decode_go_json_scalar(raw)? {
        GoJsonScalar::Null => Ok(()),
        GoJsonScalar::Number(value) => {
            *destination = value.parse().map_err(|_| EventError::InvalidField {
                field,
                expected: "signed integer or null",
            })?;
            Ok(())
        }
        _ => Err(EventError::InvalidField {
            field,
            expected: "signed integer or null",
        }),
    }
}

fn decode_go_time(raw: &[u8], destination: &mut String) -> Result<(), EventError> {
    match decode_go_json_scalar(raw)? {
        GoJsonScalar::Null => Ok(()),
        GoJsonScalar::String { raw_content, .. } => {
            *destination = normalize_go_time_unmarshal_rfc3339(raw_content).ok_or_else(|| {
                EventError::InvalidTimestamp(String::from_utf8_lossy(raw_content).into_owned())
            })?;
            Ok(())
        }
        _ => Err(EventError::InvalidField {
            field: "audited_at",
            expected: "RFC3339 string or null",
        }),
    }
}

fn decode_go_string_array(
    raw: &[u8],
    destination: &mut Vec<String>,
    field: &'static str,
) -> Result<(), EventError> {
    let Some(elements) = decode_go_json_array_elements(raw)? else {
        destination.clear();
        return Ok(());
    };
    let mut values = Vec::with_capacity(elements.len());
    for element in elements {
        let mut value = String::new();
        decode_go_string(element, &mut value, field)?;
        values.push(value);
    }
    *destination = values;
    Ok(())
}

fn decode_go_struct_array<T: Default>(
    raw: &[u8],
    destination: &mut Vec<T>,
    decode: fn(&[u8], &mut T) -> Result<(), EventError>,
) -> Result<(), EventError> {
    let Some(elements) = decode_go_json_array_elements(raw)? else {
        destination.clear();
        return Ok(());
    };
    let mut values = Vec::with_capacity(elements.len());
    for element in elements {
        let mut value = T::default();
        decode(element, &mut value)?;
        values.push(value);
    }
    *destination = values;
    Ok(())
}

fn decode_scorecard(raw: &[u8], scorecard: &mut Scorecard) -> Result<(), EventError> {
    let Some(members) = decode_go_json_object_members(raw)? else {
        return Ok(());
    };
    for member in members {
        if go_json_field_matches(&member.name, "decomposition_quality") {
            decode_go_i64(
                member.raw_value,
                &mut scorecard.decomposition_quality,
                "decomposition_quality",
            )?;
        } else if go_json_field_matches(&member.name, "persona_fit") {
            decode_go_i64(member.raw_value, &mut scorecard.persona_fit, "persona_fit")?;
        } else if go_json_field_matches(&member.name, "skill_utilization") {
            decode_go_i64(
                member.raw_value,
                &mut scorecard.skill_utilization,
                "skill_utilization",
            )?;
        } else if go_json_field_matches(&member.name, "output_quality") {
            decode_go_i64(
                member.raw_value,
                &mut scorecard.output_quality,
                "output_quality",
            )?;
        } else if go_json_field_matches(&member.name, "rule_compliance") {
            decode_go_i64(
                member.raw_value,
                &mut scorecard.rule_compliance,
                "rule_compliance",
            )?;
        } else if go_json_field_matches(&member.name, "overall") {
            decode_go_i64(member.raw_value, &mut scorecard.overall, "overall")?;
        }
    }
    Ok(())
}

fn decode_recommendation(
    raw: &[u8],
    recommendation: &mut Recommendation,
) -> Result<(), EventError> {
    let Some(members) = decode_go_json_object_members(raw)? else {
        return Ok(());
    };
    for member in members {
        if go_json_field_matches(&member.name, "category") {
            decode_go_string(member.raw_value, &mut recommendation.category, "category")?;
        } else if go_json_field_matches(&member.name, "priority") {
            decode_go_string(member.raw_value, &mut recommendation.priority, "priority")?;
        } else if go_json_field_matches(&member.name, "summary") {
            decode_go_string(member.raw_value, &mut recommendation.summary, "summary")?;
        } else if go_json_field_matches(&member.name, "detail") {
            decode_go_string(member.raw_value, &mut recommendation.detail, "detail")?;
        }
    }
    Ok(())
}

fn decode_mission_evaluation(
    raw: &[u8],
    evaluation: &mut MissionEvaluation,
) -> Result<(), EventError> {
    let Some(members) = decode_go_json_object_members(raw)? else {
        return Ok(());
    };
    for member in members {
        if go_json_field_matches(&member.name, "summary") {
            decode_go_string(member.raw_value, &mut evaluation.summary, "summary")?;
        } else if go_json_field_matches(&member.name, "strengths") {
            decode_go_string_array(member.raw_value, &mut evaluation.strengths, "strengths")?;
        } else if go_json_field_matches(&member.name, "weaknesses") {
            decode_go_string_array(member.raw_value, &mut evaluation.weaknesses, "weaknesses")?;
        } else if go_json_field_matches(&member.name, "recommendations") {
            decode_go_struct_array(
                member.raw_value,
                &mut evaluation.recommendations,
                decode_recommendation,
            )?;
        }
    }
    Ok(())
}

fn decode_phase_evaluation(raw: &[u8], phase: &mut PhaseEvaluation) -> Result<(), EventError> {
    let Some(members) = decode_go_json_object_members(raw)? else {
        return Ok(());
    };
    for member in members {
        if go_json_field_matches(&member.name, "phase_id") {
            decode_go_string(member.raw_value, &mut phase.phase_id, "phase_id")?;
        } else if go_json_field_matches(&member.name, "phase_name") {
            decode_go_string(member.raw_value, &mut phase.phase_name, "phase_name")?;
        } else if go_json_field_matches(&member.name, "persona_assigned") {
            decode_go_string(
                member.raw_value,
                &mut phase.persona_assigned,
                "persona_assigned",
            )?;
        } else if go_json_field_matches(&member.name, "persona_ideal") {
            decode_go_string(member.raw_value, &mut phase.persona_ideal, "persona_ideal")?;
        } else if go_json_field_matches(&member.name, "persona_correct") {
            decode_go_bool(
                member.raw_value,
                &mut phase.persona_correct,
                "persona_correct",
            )?;
        } else if go_json_field_matches(&member.name, "objective_met") {
            decode_go_bool(member.raw_value, &mut phase.objective_met, "objective_met")?;
        } else if go_json_field_matches(&member.name, "issues") {
            decode_go_string_array(member.raw_value, &mut phase.issues, "issues")?;
        } else if go_json_field_matches(&member.name, "score") {
            decode_go_i64(member.raw_value, &mut phase.score, "score")?;
        }
    }
    Ok(())
}

fn decode_change_record(raw: &[u8], change: &mut ChangeRecord) -> Result<(), EventError> {
    let Some(members) = decode_go_json_object_members(raw)? else {
        return Ok(());
    };
    for member in members {
        if go_json_field_matches(&member.name, "phase_id") {
            decode_go_string(member.raw_value, &mut change.phase_id, "phase_id")?;
        } else if go_json_field_matches(&member.name, "phase_name") {
            decode_go_string(member.raw_value, &mut change.phase_name, "phase_name")?;
        } else if go_json_field_matches(&member.name, "type") {
            decode_go_string(member.raw_value, &mut change.kind, "type")?;
        } else if go_json_field_matches(&member.name, "target") {
            decode_go_string(member.raw_value, &mut change.target, "target")?;
        } else if go_json_field_matches(&member.name, "summary") {
            decode_go_string(member.raw_value, &mut change.summary, "summary")?;
        }
    }
    Ok(())
}

fn decode_convergence(raw: &[u8], convergence: &mut ConvergenceStatus) -> Result<(), EventError> {
    let Some(members) = decode_go_json_object_members(raw)? else {
        return Ok(());
    };
    for member in members {
        if go_json_field_matches(&member.name, "converged") {
            decode_go_bool(member.raw_value, &mut convergence.converged, "converged")?;
        } else if go_json_field_matches(&member.name, "drift_phases") {
            decode_go_string_array(
                member.raw_value,
                &mut convergence.drift_phases,
                "drift_phases",
            )?;
        } else if go_json_field_matches(&member.name, "missing_phases") {
            decode_go_string_array(
                member.raw_value,
                &mut convergence.missing_phases,
                "missing_phases",
            )?;
        } else if go_json_field_matches(&member.name, "redundant_work") {
            decode_go_string_array(
                member.raw_value,
                &mut convergence.redundant_work,
                "redundant_work",
            )?;
        } else if go_json_field_matches(&member.name, "assessment") {
            decode_go_string(member.raw_value, &mut convergence.assessment, "assessment")?;
        }
    }
    Ok(())
}

fn decode_decomposer_convergence(
    raw: &[u8],
    convergence: &mut DecomposerConvergence,
) -> Result<(), EventError> {
    let Some(members) = decode_go_json_object_members(raw)? else {
        return Ok(());
    };
    for member in members {
        if go_json_field_matches(&member.name, "skill_md_hash") {
            decode_go_string(
                member.raw_value,
                &mut convergence.skill_md_hash,
                "skill_md_hash",
            )?;
        } else if go_json_field_matches(&member.name, "prompt_source") {
            decode_go_string(
                member.raw_value,
                &mut convergence.prompt_source,
                "prompt_source",
            )?;
        } else if go_json_field_matches(&member.name, "skill_md_path") {
            decode_go_string(
                member.raw_value,
                &mut convergence.skill_md_path,
                "skill_md_path",
            )?;
        } else if go_json_field_matches(&member.name, "rules_extracted") {
            decode_go_bool(
                member.raw_value,
                &mut convergence.rules_extracted,
                "rules_extracted",
            )?;
        }
    }
    Ok(())
}

fn decode_audit_report(raw: &[u8]) -> Result<AuditReport, EventError> {
    let mut report = AuditReport::default();
    // This first pass validates every complete member value relative to the
    // outer object, including Go's global 10,000-container depth cap. The
    // narrower nested projections below therefore cannot reset that cap.
    let Some(members) = decode_go_json_object_members(raw)? else {
        return Ok(report);
    };
    for member in members {
        if go_json_field_matches(&member.name, "workspace_id") {
            decode_go_string(member.raw_value, &mut report.workspace_id, "workspace_id")?;
        } else if go_json_field_matches(&member.name, "task") {
            decode_go_string(member.raw_value, &mut report.task, "task")?;
        } else if go_json_field_matches(&member.name, "domain") {
            decode_go_string(member.raw_value, &mut report.domain, "domain")?;
        } else if go_json_field_matches(&member.name, "status") {
            decode_go_string(member.raw_value, &mut report.status, "status")?;
        } else if go_json_field_matches(&member.name, "audited_at") {
            decode_go_time(member.raw_value, &mut report.audited_at)?;
        } else if go_json_field_matches(&member.name, "linear_issue_id") {
            decode_go_string(
                member.raw_value,
                &mut report.linear_issue_id,
                "linear_issue_id",
            )?;
        } else if go_json_field_matches(&member.name, "mission_path") {
            decode_go_string(member.raw_value, &mut report.mission_path, "mission_path")?;
        } else if go_json_field_matches(&member.name, "scorecard") {
            decode_scorecard(member.raw_value, &mut report.scorecard)?;
        } else if go_json_field_matches(&member.name, "evaluation") {
            decode_mission_evaluation(member.raw_value, &mut report.evaluation)?;
        } else if go_json_field_matches(&member.name, "phases") {
            decode_go_struct_array(
                member.raw_value,
                &mut report.phases,
                decode_phase_evaluation,
            )?;
        } else if go_json_field_matches(&member.name, "convergence") {
            decode_convergence(member.raw_value, &mut report.convergence)?;
        } else if go_json_field_matches(&member.name, "decomposer_convergence") {
            decode_decomposer_convergence(member.raw_value, &mut report.decomposer_convergence)?;
        } else if go_json_field_matches(&member.name, "changes") {
            decode_go_struct_array(member.raw_value, &mut report.changes, decode_change_record)?;
        }
    }
    Ok(report)
}

/// Reads all audit reports from `path`, in append order (oldest first).
/// Ports `LoadReports` (`store.go:52-84`): returns an empty vector (not an
/// error) if the file doesn't exist, and silently skips lines that fail to
/// parse as JSON.
///
/// `path` must be absolute (matching every observed call site — `audits.jsonl`
/// is always built from `config.Dir()`/its Rust analog). The parent directory
/// and final file component are opened through `runtime_home`'s cap_std
/// traversal and `fs_util::open_file_nofollow`, per `PORTING.md` §2.11 — never
/// a raw `std::fs::File::open` on an attacker-influenced or symlink-able path.
pub fn load_reports(path: &Path) -> Result<Vec<AuditReport>, AuditReadError> {
    let Some(parent) = path.parent() else {
        return Ok(Vec::new());
    };
    let Some(file_name) = path.file_name() else {
        return Ok(Vec::new());
    };

    let directory = match runtime_home::open_canonical_directory(parent) {
        Ok(directory) => directory,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(AuditReadError::Open { source }),
    };
    let file = match fs_util::open_file_nofollow(&directory, Path::new(file_name)) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(AuditReadError::Open { source }),
    };

    load_reports_from_reader(file)
}

/// Reads all audit reports through a retained, read-only runtime-home target.
///
/// This is the capability-safe CLI seam for `orchestrator audit scorecard`.
/// A missing leaf has the same empty-store meaning as [`load_reports`].
pub fn load_reports_from_target(
    target: &ReadOnlyEventLogTarget,
) -> Result<Vec<AuditReport>, AuditReadError> {
    let file = match target.open() {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(AuditReadError::Open { source }),
    };

    load_reports_from_reader(file)
}

fn load_reports_from_reader(
    reader: impl std::io::Read,
) -> Result<Vec<AuditReport>, AuditReadError> {
    let mut reports = Vec::new();
    let mut reader = BufReader::with_capacity(64 * 1024, reader);
    let mut line = Vec::with_capacity(64 * 1024);
    while read_go_scanner_line(&mut reader, &mut line)
        .map_err(|source| AuditReadError::Read { source })?
    {
        if let Ok(report) = decode_audit_report(&line) {
            reports.push(report);
        }
    }
    Ok(reports)
}

/// Reads one token with the same effective boundary as Go's
/// `Scanner.Buffer(make([]byte, 0, 64*1024), 1024*1024)` plus `ScanLines`.
///
/// The delimiter consumes one byte of the scanner buffer, so both LF-ended
/// and final-EOF records may contain at most one MiB minus one byte.
fn read_go_scanner_line<R: BufRead>(reader: &mut R, line: &mut Vec<u8>) -> std::io::Result<bool> {
    line.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(false);
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(true);
        }

        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            if line.len().saturating_add(newline) > GO_EVENT_JSON_CONTENT_MAX_BYTES {
                return Err(scanner_token_too_long());
            }
            line.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(true);
        }

        let remaining = GO_EVENT_JSON_CONTENT_MAX_BYTES.saturating_sub(line.len());
        if available.len() > remaining {
            return Err(scanner_token_too_long());
        }
        let consumed = available.len();
        line.extend_from_slice(available);
        reader.consume(consumed);
    }
}

fn scanner_token_too_long() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "bufio.Scanner: token too long",
    )
}

/// Mirrors Go's `MetricName` (`scorecard.go:11-21`) as a closed enum rather
/// than a bare string type, per `PORTING.md` §2.2's "keep it a matchable
/// variant" discipline for what Go treats as a typed sentinel set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MetricName {
    Decomposition,
    PersonaFit,
    SkillUsage,
    OutputQuality,
    RuleCompliance,
    Overall,
}

/// Every tracked axis in display order. Mirrors Go's `AllMetrics`
/// (`scorecard.go:24-31`).
pub const ALL_METRICS: [MetricName; 6] = [
    MetricName::Decomposition,
    MetricName::PersonaFit,
    MetricName::SkillUsage,
    MetricName::OutputQuality,
    MetricName::RuleCompliance,
    MetricName::Overall,
];

impl MetricName {
    /// The JSON/wire string Go's `MetricName` constants hold
    /// (`scorecard.go:14-20`).
    #[must_use]
    pub fn wire_value(self) -> &'static str {
        match self {
            MetricName::Decomposition => "decomposition",
            MetricName::PersonaFit => "persona_fit",
            MetricName::SkillUsage => "skill_usage",
            MetricName::OutputQuality => "output_quality",
            MetricName::RuleCompliance => "rule_compliance",
            MetricName::Overall => "overall",
        }
    }

    /// Ports `MetricLabel` (`scorecard.go:34-51`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            MetricName::Decomposition => "Decomposition",
            MetricName::PersonaFit => "Persona Fit",
            MetricName::SkillUsage => "Skill Usage",
            MetricName::OutputQuality => "Output Quality",
            MetricName::RuleCompliance => "Rule Compliance",
            MetricName::Overall => "Overall",
        }
    }
}

/// Mirrors Go's `DataPoint` (`scorecard.go:54-59`).
#[derive(Debug, Clone)]
pub struct DataPoint {
    pub workspace_id: String,
    pub audited_at: String,
    pub score: i64,
    pub domain: String,
}

/// Mirrors Go's `TrendLine` (`scorecard.go:62-71`).
#[derive(Debug, Clone)]
pub struct TrendLine {
    pub metric: MetricName,
    pub points: Vec<DataPoint>,
    pub current: i64,
    pub average: f64,
    pub trend: String,
    pub delta: i64,
    pub min: i64,
    pub max: i64,
}

/// Mirrors Go's `Regression` (`scorecard.go:74-83`).
#[derive(Debug, Clone)]
pub struct Regression {
    pub metric: MetricName,
    pub workspace_id: String,
    pub audited_at: String,
    pub prev_score: i64,
    pub new_score: i64,
    pub drop: i64,
    pub domain: String,
    pub top_issues: Vec<String>,
}

/// Mirrors Go's `ScorecardSummary` (`scorecard.go:86-91`).
#[derive(Debug, Clone, Default)]
pub struct ScorecardSummary {
    pub total_audits: i64,
    pub date_range: String,
    pub trends: Vec<TrendLine>,
    pub regressions: Vec<Regression>,
}

/// Ports `extractScore` (`scorecard.go:132-149`).
fn extract_score(scorecard: &Scorecard, metric: MetricName) -> i64 {
    match metric {
        MetricName::Decomposition => scorecard.decomposition_quality,
        MetricName::PersonaFit => scorecard.persona_fit,
        MetricName::SkillUsage => scorecard.skill_utilization,
        MetricName::OutputQuality => scorecard.output_quality,
        MetricName::RuleCompliance => scorecard.rule_compliance,
        MetricName::Overall => scorecard.overall,
    }
}

/// Ports `time.Time.Format("2006-01-02")` for a normalized timestamp.
fn date_only(rfc3339: &str) -> String {
    rfc3339
        .get(..10)
        .filter(|date| {
            date.as_bytes().get(4) == Some(&b'-') && date.as_bytes().get(7) == Some(&b'-')
        })
        .map(str::to_owned)
        .unwrap_or_else(|| "0001-01-01".to_owned())
}

/// Ports `time.Time.Format(time.RFC3339)`: Go omits fractional seconds for
/// this layout while retaining the already-normalized numeric zone. This must
/// not feed normalized values back through the input parser: Go can normalize
/// the accepted input `+24:60` to the output-only zone `+25:00`.
fn format_go_rfc3339(rfc3339_nano: &str) -> String {
    if !rfc3339_nano.is_ascii()
        || rfc3339_nano.len() < 20
        || rfc3339_nano.as_bytes().get(4) != Some(&b'-')
        || rfc3339_nano.as_bytes().get(7) != Some(&b'-')
        || rfc3339_nano.as_bytes().get(10) != Some(&b'T')
        || rfc3339_nano.as_bytes().get(13) != Some(&b':')
        || rfc3339_nano.as_bytes().get(16) != Some(&b':')
    {
        return default_go_time();
    }
    let zone_start = rfc3339_nano
        .as_bytes()
        .iter()
        .enumerate()
        .skip(19)
        .find_map(|(index, byte)| matches!(byte, b'Z' | b'+' | b'-').then_some(index));
    let Some(zone_start) = zone_start else {
        return default_go_time();
    };
    format!("{}{}", &rfc3339_nano[..19], &rfc3339_nano[zone_start..])
}

/// Ports `classifyTrend` (`scorecard.go:185-215`): least-squares linear
/// regression slope; `|slope| < 0.15` per audit is "stable".
fn classify_trend(points: &[DataPoint]) -> String {
    let n = points.len();
    if n < 2 {
        return "stable".to_string();
    }
    let (mut sum_x, mut sum_y, mut sum_xy, mut sum_x2) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
    for (i, point) in points.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let x = i as f64;
        #[allow(clippy::cast_precision_loss)]
        let y = point.score as f64;
        sum_x += x;
        sum_y += y;
        sum_xy += x * y;
        sum_x2 += x * x;
    }
    #[allow(clippy::cast_precision_loss)]
    let nf = n as f64;
    let denom = nf * sum_x2 - sum_x * sum_x;
    if denom == 0.0 {
        return "stable".to_string();
    }
    let slope = (nf * sum_xy - sum_x * sum_y) / denom;
    if slope.abs() < 0.15 {
        "stable".to_string()
    } else if slope > 0.0 {
        "improving".to_string()
    } else {
        "declining".to_string()
    }
}

/// Ports `computeTrend` (`scorecard.go:152-182`).
fn compute_trend(metric: MetricName, points: Vec<DataPoint>) -> TrendLine {
    if points.is_empty() {
        return TrendLine {
            metric,
            points,
            current: 0,
            average: 0.0,
            trend: String::new(),
            delta: 0,
            min: 0,
            max: 0,
        };
    }
    let current = points[points.len() - 1].score;
    let mut min = points[0].score;
    let mut max = points[0].score;
    let mut sum = 0_i64;
    for point in &points {
        sum += point.score;
        min = min.min(point.score);
        max = max.max(point.score);
    }
    #[allow(clippy::cast_precision_loss)]
    let average = sum as f64 / points.len() as f64;
    let delta = if points.len() >= 2 {
        points[points.len() - 1].score - points[points.len() - 2].score
    } else {
        0
    };
    let trend = classify_trend(&points);
    TrendLine {
        metric,
        points,
        current,
        average,
        trend,
        delta,
        min,
        max,
    }
}

/// Ports `detectRegressions` (`scorecard.go:218-254`).
fn detect_regressions(reports: &[AuditReport]) -> Vec<Regression> {
    if reports.len() < 2 {
        return Vec::new();
    }
    let mut regressions = Vec::new();
    for i in 1..reports.len() {
        let prev = &reports[i - 1];
        let curr = &reports[i];
        for metric in ALL_METRICS {
            let prev_score = extract_score(&prev.scorecard, metric);
            let curr_score = extract_score(&curr.scorecard, metric);
            let drop = prev_score - curr_score;
            if drop >= 1 {
                let mut issues: Vec<String> = curr.evaluation.weaknesses.clone();
                issues.truncate(3);
                regressions.push(Regression {
                    metric,
                    workspace_id: curr.workspace_id.clone(),
                    audited_at: curr.audited_at.clone(),
                    prev_score,
                    new_score: curr_score,
                    drop,
                    domain: curr.domain.clone(),
                    top_issues: issues,
                });
            }
        }
    }
    regressions
}

/// Ports `BuildScorecard` (`scorecard.go:93-129`). `reports` must already
/// be in chronological order (oldest first) — matching Go, this function
/// does not sort.
#[must_use]
pub fn build_scorecard(reports: &[AuditReport]) -> ScorecardSummary {
    if reports.is_empty() {
        return ScorecardSummary::default();
    }

    let mut summary = ScorecardSummary {
        total_audits: i64::try_from(reports.len()).unwrap_or(i64::MAX),
        ..Default::default()
    };

    let first = date_only(&reports[0].audited_at);
    let last = date_only(&reports[reports.len() - 1].audited_at);
    summary.date_range = format!("{first} to {last}");

    for metric in ALL_METRICS {
        let points: Vec<DataPoint> = reports
            .iter()
            .map(|report| DataPoint {
                workspace_id: report.workspace_id.clone(),
                audited_at: report.audited_at.clone(),
                score: extract_score(&report.scorecard, metric),
                domain: report.domain.clone(),
            })
            .collect();
        summary.trends.push(compute_trend(metric, points));
    }

    summary.regressions = detect_regressions(reports);
    summary
}

/// Ports `sparkline` (`scorecard.go:354-371`).
fn sparkline(points: &[DataPoint]) -> String {
    if points.is_empty() {
        return String::new();
    }
    const BLOCKS: [char; 5] = [' ', '.', ':', '#', '@'];
    let mut chars = String::new();
    for point in points {
        let idx = (point.score - 1).clamp(0, 4);
        #[allow(clippy::cast_sign_loss)]
        let idx = idx as usize;
        chars.push(BLOCKS[idx]);
    }
    format!("[{chars}]")
}

/// Ports `FormatScorecard` (`scorecard.go:257-351`).
#[must_use]
pub fn format_scorecard(summary: &ScorecardSummary) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    out.push_str("Audit Scorecard\n");
    out.push_str(&"=".repeat(60));
    out.push('\n');
    let _ = writeln!(
        out,
        "Audits: {}  |  Period: {}\n",
        summary.total_audits, summary.date_range
    );

    if summary.total_audits == 0 {
        out.push_str("No audit reports found. Run `orchestrator audit` first.\n");
        return out;
    }

    out.push_str("Metric Trends\n");
    out.push_str(&"-".repeat(60));
    out.push('\n');
    let _ = writeln!(
        out,
        "  {:<17}  {:<4}  {:<4}  {:<7}  {:<5}  Trend",
        "Metric", "Cur", "Avg", "Range", "Delta"
    );
    let _ = writeln!(
        out,
        "  {:<17}  {:<4}  {:<4}  {:<7}  {:<5}  ----------",
        "-".repeat(17),
        "---",
        "---",
        "-----",
        "-----",
    );

    for trend in &summary.trends {
        let delta_str = if trend.points.len() >= 2 {
            match trend.delta.cmp(&0) {
                std::cmp::Ordering::Greater => format!(" +{}", trend.delta),
                std::cmp::Ordering::Less => format!(" {}", trend.delta),
                std::cmp::Ordering::Equal => "  0".to_string(),
            }
        } else {
            "  -".to_string()
        };
        let trend_icon = match trend.trend.as_str() {
            "improving" => "^",
            "declining" => "v",
            _ => "=",
        };
        let _ = writeln!(
            out,
            "  {:<17}  {}/5  {:.1}  {} - {}  {}    {} {}",
            trend.metric.label(),
            trend.current,
            trend.average,
            trend.min,
            trend.max,
            delta_str,
            trend_icon,
            trend.trend,
        );
    }
    out.push('\n');

    let max_spark = 10_usize;
    let count = summary
        .total_audits
        .min(i64::try_from(max_spark).unwrap_or(i64::MAX));
    let _ = writeln!(out, "Score History (last {count} audits)");
    out.push_str(&"-".repeat(60));
    out.push('\n');

    for trend in &summary.trends {
        let points: &[DataPoint] = if trend.points.len() > max_spark {
            &trend.points[trend.points.len() - max_spark..]
        } else {
            &trend.points
        };
        let spark = sparkline(points);
        let _ = writeln!(out, "  {:<17}  {}", trend.metric.label(), spark);
    }
    out.push('\n');

    if summary.regressions.is_empty() {
        out.push_str("No regressions detected.\n");
    } else {
        let _ = writeln!(out, "Regressions Detected ({})", summary.regressions.len());
        out.push_str(&"-".repeat(60));
        out.push('\n');
        for regression in &summary.regressions {
            let _ = writeln!(
                out,
                "  {}: {} -> {} (-{}) in {}",
                regression.metric.label(),
                regression.prev_score,
                regression.new_score,
                regression.drop,
                regression.workspace_id,
            );
            for issue in &regression.top_issues {
                let _ = writeln!(out, "    ! {issue}");
            }
        }
    }

    out
}

#[derive(Debug, Clone, Copy)]
struct GoJsonFloat(f64);

impl Serialize for GoJsonFloat {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::Error as _;

        if !self.0.is_finite() {
            let spelling = if self.0.is_nan() {
                "NaN"
            } else if self.0.is_sign_positive() {
                "+Inf"
            } else {
                "-Inf"
            };
            return Err(S::Error::custom(format!(
                "json: unsupported value: {spelling}"
            )));
        }
        const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
        if self.0.fract() == 0.0 && self.0.abs() <= MAX_SAFE_INTEGER {
            #[allow(clippy::cast_possible_truncation)]
            return serializer.serialize_i64(self.0 as i64);
        }
        serializer.serialize_f64(self.0)
    }
}

#[derive(Debug, Serialize)]
struct JsonTrend<'a> {
    metric: &'static str,
    current: i64,
    average: GoJsonFloat,
    min: i64,
    max: i64,
    delta: i64,
    trend: &'a str,
    history: Option<Vec<i64>>,
}

#[derive(Debug, Serialize)]
struct JsonRegression<'a> {
    metric: &'static str,
    workspace_id: &'a str,
    audited_at: String,
    prev_score: i64,
    new_score: i64,
    drop: i64,
    domain: &'a str,
    top_issues: Option<&'a [String]>,
}

#[derive(Debug, Serialize)]
struct JsonScorecard<'a> {
    total_audits: i64,
    date_range: &'a str,
    trends: Option<Vec<JsonTrend<'a>>>,
    regressions: Option<Vec<JsonRegression<'a>>>,
}

/// Ports `FormatScorecardJSON` (`scorecard.go:374-443`): pretty-printed
/// JSON with `average` rounded to one decimal (`math.Round(t.Average*10)/10`).
pub fn format_scorecard_json(summary: &ScorecardSummary) -> Result<String, AuditReadError> {
    let out = JsonScorecard {
        total_audits: summary.total_audits,
        date_range: &summary.date_range,
        trends: (!summary.trends.is_empty()).then(|| {
            summary
                .trends
                .iter()
                .map(|trend| JsonTrend {
                    metric: trend.metric.wire_value(),
                    current: trend.current,
                    average: GoJsonFloat((trend.average * 10.0).round() / 10.0),
                    min: trend.min,
                    max: trend.max,
                    delta: trend.delta,
                    trend: &trend.trend,
                    history: (!trend.points.is_empty())
                        .then(|| trend.points.iter().map(|p| p.score).collect()),
                })
                .collect()
        }),
        regressions: (!summary.regressions.is_empty()).then(|| {
            summary
                .regressions
                .iter()
                .map(|regression| JsonRegression {
                    metric: regression.metric.wire_value(),
                    workspace_id: &regression.workspace_id,
                    audited_at: format_go_rfc3339(&regression.audited_at),
                    prev_score: regression.prev_score,
                    new_score: regression.new_score,
                    drop: regression.drop,
                    domain: &regression.domain,
                    // `detect_regressions` starts from a nil Go slice and an
                    // empty weaknesses append leaves it nil. Thus every
                    // build-produced empty TopIssues is `null`; a non-nil
                    // empty slice is not reachable through BuildScorecard.
                    top_issues: (!regression.top_issues.is_empty())
                        .then_some(regression.top_issues.as_slice()),
                })
                .collect()
        }),
    };

    serde_json::to_string_pretty(&out)
        .map(|json| escape_go_json_for_html(&json))
        .map_err(|source| AuditReadError::Marshal { source })
}

/// Matches `encoding/json`'s `MarshalIndent` string escaping. These code
/// points cannot occur as JSON structural syntax, so replacing them in the
/// completed document affects strings only.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn make_report(
        workspace_id: &str,
        domain: &str,
        audited_at: &str,
        scorecard: Scorecard,
    ) -> AuditReport {
        AuditReport {
            workspace_id: workspace_id.to_string(),
            task: format!("Test task for {workspace_id}"),
            domain: domain.to_string(),
            status: "completed".to_string(),
            audited_at: audited_at.to_string(),
            scorecard,
            evaluation: MissionEvaluation {
                summary: "Test evaluation".to_string(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn exact_length_report_line(length: usize) -> Vec<u8> {
        let prefix = br#"{"workspace_id":""#;
        let suffix = br#"","audited_at":"2026-07-17T01:02:03Z"}"#;
        let padding = length - prefix.len() - suffix.len();
        let mut line = Vec::with_capacity(length);
        line.extend_from_slice(prefix);
        line.resize(line.len() + padding, b'x');
        line.extend_from_slice(suffix);
        line
    }

    fn nested_unknown_report_line(workspace_id: &str, array_depth: usize) -> Vec<u8> {
        let mut line = format!(r#"{{"workspace_id":"{workspace_id}","unknown":"#).into_bytes();
        line.resize(line.len() + array_depth, b'[');
        line.extend_from_slice(b"null");
        line.resize(line.len() + array_depth, b']');
        line.push(b'}');
        line
    }

    /// Ports `TestBuildScorecardEmpty` (`scorecard_test.go:15-24`).
    #[test]
    fn build_scorecard_empty() {
        let summary = build_scorecard(&[]);
        assert_eq!(summary.total_audits, 0);
        assert!(summary.trends.is_empty());
    }

    /// Ports `TestBuildScorecardSingle` (`scorecard_test.go:25-57`).
    #[test]
    fn build_scorecard_single_report() {
        let r1 = make_report(
            "ws-1",
            "dev",
            "2026-02-20T00:00:00Z",
            Scorecard {
                decomposition_quality: 4,
                persona_fit: 3,
                skill_utilization: 2,
                output_quality: 4,
                rule_compliance: 3,
                overall: 3,
            },
        );
        let summary = build_scorecard(&[r1]);
        assert_eq!(summary.total_audits, 1);
        assert_eq!(summary.trends.len(), 6);

        let decomposition_trend = &summary.trends[0];
        assert_eq!(decomposition_trend.current, 4);
        assert_eq!(decomposition_trend.average, 4.0);
        assert_eq!(decomposition_trend.trend, "stable");
        assert_eq!(decomposition_trend.delta, 0);

        assert!(summary.regressions.is_empty());
    }

    /// Ports `TestFormatScorecard` (`scorecard_test.go:217-232`).
    #[test]
    fn format_scorecard_non_empty_contains_expected_sections() {
        let r1 = make_report(
            "ws-1",
            "dev",
            "2026-02-20T00:00:00Z",
            Scorecard {
                decomposition_quality: 3,
                persona_fit: 3,
                skill_utilization: 3,
                output_quality: 3,
                rule_compliance: 3,
                overall: 3,
            },
        );
        let r2 = make_report(
            "ws-2",
            "dev",
            "2026-02-22T00:00:00Z",
            Scorecard {
                decomposition_quality: 4,
                persona_fit: 4,
                skill_utilization: 4,
                output_quality: 4,
                rule_compliance: 4,
                overall: 4,
            },
        );
        let summary = build_scorecard(&[r1, r2]);
        let text = format_scorecard(&summary);

        for want in [
            "Audit Scorecard",
            "Metric Trends",
            "Decomposition",
            "Score History",
            "regressions",
        ] {
            assert!(text.contains(want), "formatted text missing {want:?}");
        }
    }

    /// Ports `TestFormatScorecardEmpty` (`scorecard_test.go:311-317`).
    #[test]
    fn format_scorecard_empty_says_no_reports() {
        let summary = build_scorecard(&[]);
        let text = format_scorecard(&summary);
        assert!(text.contains("No audit reports found"));
    }

    /// Ports `TestClassifyTrend` (`scorecard_test.go:136-164`).
    #[test]
    fn classify_trend_cases() {
        let point = |score: i64| DataPoint {
            workspace_id: String::new(),
            audited_at: String::new(),
            score,
            domain: String::new(),
        };
        assert_eq!(classify_trend(&[]), "stable");
        assert_eq!(classify_trend(&[point(3)]), "stable");
        assert_eq!(classify_trend(&[point(3), point(3), point(3)]), "stable");
        assert_eq!(
            classify_trend(&[point(1), point(2), point(3), point(4)]),
            "improving"
        );
        assert_eq!(
            classify_trend(&[point(4), point(3), point(2), point(1)]),
            "declining"
        );
    }

    /// Ports `TestExtractScore` (`scorecard_test.go:165-193`).
    #[test]
    fn extract_score_reads_each_axis() {
        let scorecard = Scorecard {
            decomposition_quality: 1,
            persona_fit: 2,
            skill_utilization: 3,
            output_quality: 4,
            rule_compliance: 5,
            overall: 3,
        };
        assert_eq!(extract_score(&scorecard, MetricName::Decomposition), 1);
        assert_eq!(extract_score(&scorecard, MetricName::PersonaFit), 2);
        assert_eq!(extract_score(&scorecard, MetricName::SkillUsage), 3);
        assert_eq!(extract_score(&scorecard, MetricName::OutputQuality), 4);
        assert_eq!(extract_score(&scorecard, MetricName::RuleCompliance), 5);
        assert_eq!(extract_score(&scorecard, MetricName::Overall), 3);
    }

    /// Ports `TestSparkline` (`scorecard_test.go:194-216`).
    #[test]
    fn sparkline_maps_scores_to_blocks() {
        let point = |score: i64| DataPoint {
            workspace_id: String::new(),
            audited_at: String::new(),
            score,
            domain: String::new(),
        };
        assert_eq!(sparkline(&[]), "");
        assert_eq!(
            sparkline(&[point(1), point(3), point(5)]),
            "[ :@]".to_string()
        );
        // Out-of-range scores clamp rather than panic.
        assert_eq!(sparkline(&[point(0), point(99)]), "[ @]".to_string());
    }

    /// Ports `TestBuildScorecardTrends` + `TestDetectRegressions`
    /// (`scorecard_test.go:59-135`).
    #[test]
    fn build_scorecard_trends_and_regressions() {
        let r1 = make_report(
            "ws-1",
            "dev",
            "2026-02-20T00:00:00Z",
            Scorecard {
                decomposition_quality: 5,
                persona_fit: 5,
                skill_utilization: 5,
                output_quality: 5,
                rule_compliance: 5,
                overall: 5,
            },
        );
        let mut r2 = make_report(
            "ws-2",
            "dev",
            "2026-02-22T00:00:00Z",
            Scorecard {
                decomposition_quality: 3,
                persona_fit: 3,
                skill_utilization: 3,
                output_quality: 3,
                rule_compliance: 3,
                overall: 3,
            },
        );
        r2.evaluation.weaknesses = vec!["w1".to_string(), "w2".to_string()];

        let summary = build_scorecard(&[r1, r2]);
        assert_eq!(summary.total_audits, 2);
        assert_eq!(summary.date_range, "2026-02-20 to 2026-02-22");
        assert_eq!(summary.trends.len(), 6);
        assert_eq!(summary.regressions.len(), 6); // every axis dropped by 2
        for regression in &summary.regressions {
            assert_eq!(regression.drop, 2);
            assert_eq!(
                regression.top_issues,
                vec!["w1".to_string(), "w2".to_string()]
            );
        }
    }

    /// Ports `TestRegressionTopIssuesCapped` (`scorecard_test.go:319-332`).
    #[test]
    fn regression_top_issues_capped_at_three() {
        let r1 = make_report(
            "ws-1",
            "dev",
            "2026-02-20T00:00:00Z",
            Scorecard {
                decomposition_quality: 5,
                persona_fit: 5,
                skill_utilization: 5,
                output_quality: 5,
                rule_compliance: 5,
                overall: 5,
            },
        );
        let mut r2 = make_report(
            "ws-2",
            "dev",
            "2026-02-22T00:00:00Z",
            Scorecard {
                decomposition_quality: 1,
                persona_fit: 1,
                skill_utilization: 1,
                output_quality: 1,
                rule_compliance: 1,
                overall: 1,
            },
        );
        r2.evaluation.weaknesses = vec![
            "w1".into(),
            "w2".into(),
            "w3".into(),
            "w4".into(),
            "w5".into(),
        ];

        let summary = build_scorecard(&[r1, r2]);
        for regression in &summary.regressions {
            assert!(regression.top_issues.len() <= 3);
        }
    }

    /// Ports `TestFormatScorecardJSON` (`scorecard_test.go:234-253`).
    #[test]
    fn format_scorecard_json_rounds_average_to_one_decimal() -> TestResult {
        let r1 = make_report(
            "ws-1",
            "dev",
            "2026-02-20T00:00:00Z",
            Scorecard {
                decomposition_quality: 4,
                persona_fit: 4,
                skill_utilization: 4,
                output_quality: 4,
                rule_compliance: 4,
                overall: 4,
            },
        );
        let r2 = make_report(
            "ws-2",
            "dev",
            "2026-02-22T00:00:00Z",
            Scorecard {
                decomposition_quality: 5,
                persona_fit: 5,
                skill_utilization: 5,
                output_quality: 5,
                rule_compliance: 5,
                overall: 5,
            },
        );
        let summary = build_scorecard(&[r1, r2]);
        let json = format_scorecard_json(&summary)?;
        let value: serde_json::Value = serde_json::from_str(&json)?;
        let trends = value["trends"].as_array().ok_or("expected trends array")?;
        let overall_trend = trends
            .iter()
            .find(|t| t["metric"] == "overall")
            .ok_or("expected overall trend")?;
        assert_eq!(overall_trend["average"], serde_json::json!(4.5));
        Ok(())
    }

    #[test]
    fn format_scorecard_json_renders_integral_averages_as_go_numbers() -> TestResult {
        let report = make_report(
            "ws-1",
            "dev",
            "2026-02-20T00:00:00Z",
            Scorecard {
                decomposition_quality: 3,
                persona_fit: 3,
                skill_utilization: 3,
                output_quality: 3,
                rule_compliance: 3,
                overall: 3,
            },
        );

        let json = format_scorecard_json(&build_scorecard(&[report]))?;

        assert!(json.contains("\"average\": 3,"));
        assert!(!json.contains("\"average\": 3.0"));
        Ok(())
    }

    #[test]
    fn format_scorecard_json_uses_null_for_go_nil_slices() -> TestResult {
        let json = format_scorecard_json(&build_scorecard(&[]))?;
        let value: serde_json::Value = serde_json::from_str(&json)?;

        assert!(value["trends"].is_null());
        assert!(value["regressions"].is_null());
        Ok(())
    }

    #[test]
    fn loaded_plus_24_60_time_survives_build_text_and_json() -> TestResult {
        let temp =
            std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
        let dir = temp.join(format!("audit-read-test-plus-24-60-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("audits.jsonl");
        let mut file = std::fs::File::create(&path)?;
        writeln!(
            file,
            r#"{{"workspace_id":"ws-1","audited_at":"2026-07-16T01:02:03Z","scorecard":{{"decomposition_quality":5,"persona_fit":5,"skill_utilization":5,"output_quality":5,"rule_compliance":5,"overall":5}}}}"#
        )?;
        writeln!(
            file,
            r#"{{"workspace_id":"ws-2","audited_at":"2026-07-17T01:02:03.12+24:60","scorecard":{{"decomposition_quality":4,"persona_fit":4,"skill_utilization":4,"output_quality":4,"rule_compliance":4,"overall":4}}}}"#
        )?;
        drop(file);

        let reports = load_reports(&path)?;
        assert_eq!(reports[1].audited_at, "2026-07-17T01:02:03.12+25:00");
        let summary = build_scorecard(&reports);
        assert_eq!(summary.date_range, "2026-07-16 to 2026-07-17");
        assert!(format_scorecard(&summary).contains("2026-07-16 to 2026-07-17"));
        let json = format_scorecard_json(&summary)?;
        let value: serde_json::Value = serde_json::from_str(&json)?;
        let regressions = value["regressions"]
            .as_array()
            .ok_or("expected regressions array")?;

        assert_eq!(
            regressions[0]["audited_at"],
            serde_json::json!("2026-07-17T01:02:03+25:00")
        );
        assert!(regressions[0]["top_issues"].is_null());

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn format_scorecard_json_matches_go_html_escaping_bytes() -> TestResult {
        let summary = ScorecardSummary {
            total_audits: 1,
            date_range: "2026-07-17 to 2026-07-17".to_owned(),
            trends: Vec::new(),
            regressions: vec![Regression {
                metric: MetricName::Overall,
                workspace_id: "<ws>&\u{2028}\u{2029}".to_owned(),
                audited_at: "2026-07-17T01:02:03Z".to_owned(),
                prev_score: 5,
                new_score: 4,
                drop: 1,
                domain: "<dev>&\u{2028}\u{2029}".to_owned(),
                top_issues: vec!["<issue>&\u{2028}\u{2029}".to_owned()],
            }],
        };
        let expected = r#"{
  "total_audits": 1,
  "date_range": "2026-07-17 to 2026-07-17",
  "trends": null,
  "regressions": [
    {
      "metric": "overall",
      "workspace_id": "\u003cws\u003e\u0026\u2028\u2029",
      "audited_at": "2026-07-17T01:02:03Z",
      "prev_score": 5,
      "new_score": 4,
      "drop": 1,
      "domain": "\u003cdev\u003e\u0026\u2028\u2029",
      "top_issues": [
        "\u003cissue\u003e\u0026\u2028\u2029"
      ]
    }
  ]
}"#;

        assert_eq!(format_scorecard_json(&summary)?, expected);
        Ok(())
    }

    #[test]
    fn format_scorecard_json_rejects_nonfinite_average() -> TestResult {
        let summary = ScorecardSummary {
            total_audits: 1,
            date_range: "2026-07-17 to 2026-07-17".to_owned(),
            trends: vec![TrendLine {
                metric: MetricName::Overall,
                points: vec![DataPoint {
                    workspace_id: "ws".to_owned(),
                    audited_at: "2026-07-17T01:02:03Z".to_owned(),
                    score: 1,
                    domain: "dev".to_owned(),
                }],
                current: 1,
                average: f64::NAN,
                trend: "stable".to_owned(),
                delta: 0,
                min: 1,
                max: 1,
            }],
            regressions: Vec::new(),
        };

        let error = match format_scorecard_json(&summary) {
            Ok(_) => return Err("NaN average must fail like encoding/json".into()),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "marshaling scorecard: json: unsupported value: NaN"
        );
        Ok(())
    }

    /// Ports `TestSaveAndLoadReports` (`scorecard_test.go:254-292`), minus
    /// `SaveReport` (write path, not ported) — writes the fixture directly.
    #[test]
    fn load_reports_returns_append_order() -> TestResult {
        let temp =
            std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
        let dir = temp.join(format!("audit-read-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("audits.jsonl");
        let mut file = std::fs::File::create(&path)?;
        writeln!(
            file,
            r#"{{"workspace_id":"ws-1","domain":"dev","scorecard":{{"overall":3}}}}"#
        )?;
        writeln!(
            file,
            r#"{{"workspace_id":"ws-2","domain":"personal","scorecard":{{"overall":4}}}}"#
        )?;
        drop(file);

        let reports = load_reports(&path)?;
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].workspace_id, "ws-1");
        assert_eq!(reports[1].workspace_id, "ws-2");
        assert_eq!(reports[1].scorecard.overall, 4);

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// Ports `TestLoadReportsEmpty` (`scorecard_test.go:294-309`).
    #[test]
    fn load_reports_missing_file_returns_empty() -> TestResult {
        let path = Path::new("/nonexistent/does-not-exist/audits.jsonl");
        let reports = load_reports(path)?;
        assert!(reports.is_empty());
        Ok(())
    }

    /// Malformed-line tolerance, matching `store.go:74-76`'s
    /// `continue // skip malformed lines`.
    #[test]
    fn load_reports_skips_malformed_lines() -> TestResult {
        let temp =
            std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
        let dir = temp.join(format!("audit-read-test-malformed-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("audits.jsonl");
        let mut file = std::fs::File::create(&path)?;
        writeln!(file, r#"{{"workspace_id":"ws-1"}}"#)?;
        writeln!(file, "not json at all")?;
        writeln!(file, r#"{{"workspace_id":"ws-2"}}"#)?;
        drop(file);

        let reports = load_reports(&path)?;
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].workspace_id, "ws-1");
        assert_eq!(reports[1].workspace_id, "ws-2");

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn load_reports_accepts_explicit_null_slices_and_zero_time() -> TestResult {
        let temp =
            std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
        let dir = temp.join(format!(
            "audit-read-test-null-slices-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("audits.jsonl");
        let mut file = std::fs::File::create(&path)?;
        writeln!(
            file,
            r#"{{"workspace_id":"ws-null","audited_at":null,"evaluation":{{"strengths":null,"weaknesses":null,"recommendations":null}},"phases":null,"convergence":{{"drift_phases":null,"missing_phases":null,"redundant_work":null}},"changes":null}}"#
        )?;
        drop(file);

        let reports = load_reports(&path)?;
        let report = reports.first().ok_or("expected one report")?;
        assert_eq!(report.audited_at, GO_ZERO_TIME);
        assert!(report.evaluation.strengths.is_empty());
        assert!(report.evaluation.weaknesses.is_empty());
        assert!(report.evaluation.recommendations.is_empty());
        assert!(report.phases.is_empty());
        assert!(report.convergence.drift_phases.is_empty());
        assert!(report.convergence.missing_phases.is_empty());
        assert!(report.convergence.redundant_work.is_empty());
        assert!(report.changes.is_empty());

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn load_reports_normalizes_go_time_and_skips_invalid_time() -> TestResult {
        let temp =
            std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
        let dir = temp.join(format!("audit-read-test-go-time-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("audits.jsonl");
        let mut file = std::fs::File::create(&path)?;
        writeln!(
            file,
            r#"{{"workspace_id":"invalid","audited_at":"not-a-time"}}"#
        )?;
        writeln!(
            file,
            r#"{{"workspace_id":"escaped","audited_at":"2026-07-17T01:02:03\u005a"}}"#
        )?;
        writeln!(
            file,
            r#"{{"workspace_id":"valid","audited_at":"2026-07-17T1:02:03,120000000+00:60"}}"#
        )?;
        drop(file);

        let reports = load_reports(&path)?;

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].workspace_id, "valid");
        assert_eq!(reports[0].audited_at, "2026-07-17T01:02:03.12+01:00");

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn load_reports_matches_go_object_merge_and_scalar_null_semantics() -> TestResult {
        let temp =
            std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
        let dir = temp.join(format!("audit-read-test-go-object-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("audits.jsonl");
        let mut file = std::fs::File::create(&path)?;
        writeln!(
            file,
            r#"{{"WORKSPACE_ID":"first","workspace_id":null,"Workspace_Id":"last","TASK":"kept","task":null,"DOMAIN":"dev","STATUS":"completed","AUDITED_AT":"2026-07-17T01:02:03+24:60","SCORECARD":{{"OVERALL":5,"overall":null,"PERSONA_FIT":2}},"scorecard":{{"overall":3}},"EVALUATION":{{"SUMMARY":"summary","summary":null,"STRENGTHS":["one"],"strengths":null}},"PHASES":[{{"SCORE":5,"score":null,"PERSONA_CORRECT":true,"persona_correct":null}}],"CHANGES":[{{"type":"first"}}],"changes":null}}"#
        )?;
        drop(file);

        let reports = load_reports(&path)?;
        let report = reports.first().ok_or("expected one report")?;

        assert_eq!(report.workspace_id, "last");
        assert_eq!(report.task, "kept");
        assert_eq!(report.domain, "dev");
        assert_eq!(report.status, "completed");
        assert_eq!(report.audited_at, "2026-07-17T01:02:03+25:00");
        assert_eq!(report.scorecard.overall, 3);
        assert_eq!(report.scorecard.persona_fit, 2);
        assert_eq!(report.evaluation.summary, "summary");
        assert!(report.evaluation.strengths.is_empty());
        assert_eq!(report.phases[0].score, 5);
        assert!(report.phases[0].persona_correct);
        assert!(report.changes.is_empty());

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn load_reports_replaces_invalid_utf8_like_go_encoding_json() -> TestResult {
        let temp =
            std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
        let dir = temp.join(format!(
            "audit-read-test-invalid-utf8-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("audits.jsonl");
        let mut file = std::fs::File::create(&path)?;
        file.write_all(br#"{"workspace_id":"bad-"#)?;
        file.write_all(&[0xff])?;
        file.write_all(br#"-name","audited_at":"2026-07-17T01:02:03Z"}"#)?;
        file.write_all(b"\n")?;
        drop(file);

        let reports = load_reports(&path)?;

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].workspace_id, "bad-\u{fffd}-name");

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn load_reports_preserves_go_global_json_depth_limit() -> TestResult {
        let temp =
            std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
        let dir = temp.join(format!("audit-read-test-json-depth-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("audits.jsonl");
        let mut file = std::fs::File::create(&path)?;
        file.write_all(&nested_unknown_report_line("accepted", 9_999))?;
        file.write_all(b"\n")?;
        file.write_all(&nested_unknown_report_line("rejected", 10_000))?;
        file.write_all(b"\n")?;
        drop(file);

        let reports = load_reports(&path)?;

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].workspace_id, "accepted");

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn load_reports_accepts_go_scanner_maximum_content() -> TestResult {
        let temp =
            std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
        let dir = temp.join(format!("audit-read-test-max-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("audits.jsonl");
        let mut file = std::fs::File::create(&path)?;
        file.write_all(&exact_length_report_line(GO_EVENT_JSON_CONTENT_MAX_BYTES))?;
        file.write_all(b"\n")?;
        drop(file);

        let reports = load_reports(&path)?;

        assert_eq!(reports.len(), 1);

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn load_reports_accepts_go_scanner_maximum_content_at_eof() -> TestResult {
        let temp =
            std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
        let dir = temp.join(format!(
            "audit-read-test-max-token-eof-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("audits.jsonl");
        let mut file = std::fs::File::create(&path)?;
        file.write_all(&exact_length_report_line(GO_EVENT_JSON_CONTENT_MAX_BYTES))?;
        drop(file);

        let reports = load_reports(&path)?;

        assert_eq!(reports.len(), 1);

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn load_reports_rejects_token_at_go_scanner_ceiling() -> TestResult {
        let temp =
            std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
        let dir = temp.join(format!(
            "audit-read-test-oversize-token-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("audits.jsonl");
        let mut file = std::fs::File::create(&path)?;
        writeln!(
            file,
            r#"{{"workspace_id":"would-have-loaded","audited_at":"2026-07-17T01:02:03Z"}}"#
        )?;
        file.write_all(&exact_length_report_line(
            GO_EVENT_JSON_CONTENT_MAX_BYTES + 1,
        ))?;
        file.write_all(b"\n")?;
        drop(file);

        let error = match load_reports(&path) {
            Ok(_) => return Err("one-MiB scanner token must fail".into()),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "reading audits file: bufio.Scanner: token too long"
        );

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn load_reports_accepts_null_and_empty_object_as_zero_reports() -> TestResult {
        let temp =
            std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
        let dir = temp.join(format!(
            "audit-read-test-zero-reports-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("audits.jsonl");
        let mut file = std::fs::File::create(&path)?;
        writeln!(file, "null")?;
        writeln!(file, "{{}}")?;
        drop(file);

        let reports = load_reports(&path)?;

        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].audited_at, GO_ZERO_TIME);
        assert_eq!(reports[1].audited_at, GO_ZERO_TIME);

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
