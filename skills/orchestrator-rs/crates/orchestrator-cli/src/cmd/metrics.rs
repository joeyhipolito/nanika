//! Ports Go's `orchestrator metrics` command family
//! (`internal/cmd/metrics.go`).
//!
//! Every query goes through an injected `orchestrator_app::MetricsQueryService`.
//! This module parses CLI flags and formats typed rows byte-identical to Go's
//! `fmt.Printf` column layouts; it has no filesystem or SQLite capability.
//!
//! **Flagged, not ported** (per `PORT-ORDER.md` Wave 3 and
//! `orchestrator_app::metrics_read`'s own module doc comment):
//! - Go's `openMetricsDB` backfills `metrics.jsonl` into `metrics.db` before
//!   every query (`internal/cmd/metrics.go:69-88`, `ImportMissingFromJSONL`).
//!   The future owner implementation must reproduce that view privately; the
//!   Stage A production composition fails closed until it can do so safely.
//! - `metrics routing` (`showRoutingMetrics`, distinct from `routing-methods`)
//!   reads `routing_decisions` via `routing.OpenDB(...).GetPersonaRoutingStats`
//!   — a different SQLite surface (routing/learnings db) with no Rust reader
//!   in this cell's pre-approved scope. This subcommand returns
//!   [`MetricsError::RoutingReaderNotPorted`] rather than silent wrong output.

use std::io::Write;

use orchestrator_app::{
    FALLBACK_ALERT_THRESHOLD, MetricsQueryError, MetricsQueryService, MissionsQuery,
    PersonaMetricsQuery, PhasesQuery, RoutingMethodsQuery, SkillUsageQuery, TrendsQuery,
    fallback_rate,
};
use thiserror::Error;

use super::truncate;

#[derive(Debug, Error)]
pub(crate) enum MetricsError {
    #[error("querying missions: {0}")]
    QueryMissions(#[source] MetricsQueryError),
    #[error("querying persona metrics: {0}")]
    QueryPersonaMetrics(#[source] MetricsQueryError),
    #[error("querying skill usage: {0}")]
    QuerySkillUsage(#[source] MetricsQueryError),
    #[error("querying trends: {0}")]
    QueryTrends(#[source] MetricsQueryError),
    #[error("querying phases: {0}")]
    QueryPhases(#[source] MetricsQueryError),
    #[error("querying routing method distribution: {0}")]
    QueryRoutingMethodDistribution(#[source] MetricsQueryError),
    #[error(
        "metrics routing is not yet ported: it reads routing_decisions via a separate \
         routing/learnings-db reader outside this cell's pre-approved scope (see \
         PORT-ORDER.md Wave 4) — flagged rather than stubbed"
    )]
    RoutingReaderNotPorted,
    #[error("cannot write command output: {0}")]
    Output(#[from] std::io::Error),
}

/// Which `metrics` subcommand to run, with its own flags (mirrors
/// `internal/cmd/metrics.go`'s `cobra.Command` tree).
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum MetricsCommand {
    Missions(MissionsFlags),
    Personas,
    Skills,
    Trends { days: i64 },
    Routing,
    RoutingMethods,
    Phases { workspace_id: String },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct MissionsFlags {
    pub(crate) last: i64,
    pub(crate) domain: String,
    pub(crate) status: String,
    pub(crate) days: i64,
    pub(crate) decomp_source: String,
    pub(crate) worker: String,
}

impl Default for MissionsFlags {
    fn default() -> Self {
        Self {
            last: 20,
            domain: String::new(),
            status: String::new(),
            days: 0,
            decomp_source: String::new(),
            worker: String::new(),
        }
    }
}

pub(crate) fn run<S: MetricsQueryService + ?Sized>(
    service: &S,
    command: &MetricsCommand,
    output: &mut impl Write,
) -> Result<(), MetricsError> {
    match command {
        MetricsCommand::Missions(flags) => show_missions(service, flags, output),
        MetricsCommand::Personas => show_persona_metrics(service, output),
        MetricsCommand::Skills => show_skill_metrics(service, output),
        MetricsCommand::Trends { days } => show_trends(service, *days, output),
        MetricsCommand::Routing => Err(MetricsError::RoutingReaderNotPorted),
        MetricsCommand::RoutingMethods => show_routing_method_metrics(service, output),
        MetricsCommand::Phases { workspace_id } => {
            show_phase_metrics(service, workspace_id, output)
        }
    }
}

/// Ports `showMetrics` (`internal/cmd/metrics.go:98-158`).
fn show_missions<S: MetricsQueryService + ?Sized>(
    service: &S,
    flags: &MissionsFlags,
    output: &mut impl Write,
) -> Result<(), MetricsError> {
    let missions = service
        .query_missions(MissionsQuery {
            limit: flags.last,
            domain: flags.domain.clone(),
            days: flags.days,
            status: flags.status.clone(),
            decomp_source: flags.decomp_source.clone(),
            worker: flags.worker.clone(),
        })
        .map_err(MetricsError::QueryMissions)?
        .missions;
    if missions.is_empty() {
        writeln!(output, "no missions recorded yet")?;
        return Ok(());
    }

    writeln!(
        output,
        "{:<12}  {:<10}  {:<8}  {:<14}  {:<22}  {:<8} {:<12}  task",
        "workspace", "domain", "status", "decomp", "persona", "duration", "phases"
    )?;
    writeln!(output, "{}", "-".repeat(114))?;

    let mut total_duration = 0i64;
    let mut successes = 0i64;
    let mut failures = 0i64;
    for mission in &missions {
        let duration = format!("{}s", mission.duration_sec);
        let mut phases = format!("{}/{}", mission.phases_completed, mission.phases_total);
        if mission.phases_failed > 0 {
            phases.push_str(&format!("({}f)", mission.phases_failed));
        }
        let task = truncate(&mission.task.replace('\n', " "), 50);
        let persona = truncate(&mission.top_persona, 22);
        let workspace_id = truncate(&mission.workspace_id, 12);
        let decomp = truncate(&mission.decomp_source, 14);
        writeln!(
            output,
            "{workspace_id:<12}  {:<10}  {:<8}  {decomp:<14}  {persona:<22}  {duration:<8} {phases:<12}  {task}",
            mission.domain, mission.status,
        )?;
        total_duration += mission.duration_sec;
        if mission.status == "success" {
            successes += 1;
        } else if mission.status == "failed" {
            failures += 1;
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    let count = missions.len() as i64;
    let average_duration = if count > 0 { total_duration / count } else { 0 };
    writeln!(
        output,
        "\n{count} missions  \u{2022}  {successes} succeeded  \u{2022}  {failures} failed  \u{2022}  avg {average_duration}s"
    )?;
    Ok(())
}

/// Ports `showPersonaMetrics` (`internal/cmd/metrics.go:160-191`).
fn show_persona_metrics<S: MetricsQueryService + ?Sized>(
    service: &S,
    output: &mut impl Write,
) -> Result<(), MetricsError> {
    let personas = service
        .query_persona_metrics(PersonaMetricsQuery)
        .map_err(MetricsError::QueryPersonaMetrics)?
        .personas;
    if personas.is_empty() {
        writeln!(output, "no persona data recorded yet")?;
        return Ok(());
    }

    writeln!(
        output,
        "{:<30}  {:>6}  {:>8}  {:>6}  {:>9}  {:>5}  {:>5}",
        "persona", "phases", "avg_dur", "fail%", "avg_retry", "llm%", "kw%"
    )?;
    writeln!(output, "{}", "-".repeat(80))?;
    for persona in &personas {
        let name = truncate(&persona.persona, 30);
        writeln!(
            output,
            "{name:<30}  {:>6}  {:>7.0}s  {:>5.1}%  {:>9.2}  {:>4.0}%  {:>4.0}%",
            persona.phase_count,
            persona.avg_duration_sec,
            persona.failure_rate,
            persona.avg_retries,
            persona.llm_pct,
            persona.keyword_pct,
        )?;
    }
    Ok(())
}

/// Ports `showSkillMetrics` (`internal/cmd/metrics.go:193-224`).
fn show_skill_metrics<S: MetricsQueryService + ?Sized>(
    service: &S,
    output: &mut impl Write,
) -> Result<(), MetricsError> {
    let skills = service
        .query_skill_usage(SkillUsageQuery)
        .map_err(MetricsError::QuerySkillUsage)?
        .skills;
    if skills.is_empty() {
        writeln!(output, "no skill invocations recorded yet")?;
        return Ok(());
    }

    writeln!(
        output,
        "{:<28}  {:<20}  {:<24}  {:<12}  uses",
        "skill", "phase", "persona", "source"
    )?;
    writeln!(output, "{}", "-".repeat(96))?;
    for skill in &skills {
        let name = truncate(&skill.skill_name, 28);
        let phase = truncate(&skill.phase, 20);
        let persona = truncate(&skill.persona, 24);
        let source = truncate(&skill.source, 12);
        writeln!(
            output,
            "{name:<28}  {phase:<20}  {persona:<24}  {source:<12}  {}",
            skill.invocations
        )?;
    }
    Ok(())
}

/// Ports `showTrends` (`internal/cmd/metrics.go:226-275`). `db.QueryTrends`
/// clamps `days<=0` to 30 internally for the SQL query, but Go's
/// `showTrends` prints the empty-result message with the raw, unclamped
/// `days` value (`metrics.go:238-244`) — preserved here rather than using
/// the clamped value.
fn show_trends<S: MetricsQueryService + ?Sized>(
    service: &S,
    days: i64,
    output: &mut impl Write,
) -> Result<(), MetricsError> {
    let trends = service
        .query_trends(TrendsQuery { days })
        .map_err(MetricsError::QueryTrends)?
        .trends;
    if trends.is_empty() {
        writeln!(output, "no missions in the last {days} days")?;
        return Ok(());
    }

    writeln!(
        output,
        "{:<10}  {:>8}  {:>9}  {:>8}",
        "day", "missions", "success%", "avg_dur"
    )?;
    writeln!(output, "{}", "-".repeat(42))?;

    let mut total_missions = 0i64;
    let mut total_success = 0i64;
    let mut total_duration = 0.0f64;
    for trend in &trends {
        let success_pct = if trend.total > 0 {
            trend.successes as f64 / trend.total as f64 * 100.0
        } else {
            0.0
        };
        writeln!(
            output,
            "{:<10}  {:>8}  {success_pct:>8.1}%  {:>7.0}s",
            trend.day, trend.total, trend.avg_duration
        )?;
        total_missions += trend.total;
        total_success += trend.successes;
        total_duration += trend.avg_duration * trend.total as f64;
    }
    let overall_pct = if total_missions > 0 {
        total_success as f64 / total_missions as f64 * 100.0
    } else {
        0.0
    };
    let average_duration = if total_missions > 0 {
        total_duration / total_missions as f64
    } else {
        0.0
    };
    writeln!(
        output,
        "\n{} days  \u{2022}  {total_missions} missions  \u{2022}  {overall_pct:.1}% success  \u{2022}  avg {average_duration:.0}s",
        trends.len()
    )?;
    Ok(())
}

/// Ports `showRoutingMethodMetrics` (`internal/cmd/metrics.go:323-358`).
fn show_routing_method_metrics<S: MetricsQueryService + ?Sized>(
    service: &S,
    output: &mut impl Write,
) -> Result<(), MetricsError> {
    let distribution = service
        .query_routing_methods(RoutingMethodsQuery)
        .map_err(MetricsError::QueryRoutingMethodDistribution)?
        .methods;
    if distribution.is_empty() {
        writeln!(output, "no routing method data recorded yet")?;
        return Ok(());
    }

    writeln!(output, "{:<20}  {:>8}  {:>8}", "method", "phases", "pct%")?;
    writeln!(output, "{}", "-".repeat(42))?;
    let mut total = 0i64;
    for row in &distribution {
        writeln!(
            output,
            "{:<20}  {:>8}  {:>7.1}%",
            row.method, row.count, row.pct
        )?;
        total += row.count;
    }
    writeln!(output, "\n{total} phases total")?;

    let fallback = fallback_rate(&distribution);
    if fallback > FALLBACK_ALERT_THRESHOLD {
        writeln!(
            output,
            "\nALERT: fallback routing rate {fallback:.1}% exceeds {FALLBACK_ALERT_THRESHOLD:.0}% threshold — \
             LLM decomposition may be failing too often"
        )?;
    }
    Ok(())
}

/// Ports `showPhaseMetrics` (`internal/cmd/metrics.go:360-396`).
fn show_phase_metrics<S: MetricsQueryService + ?Sized>(
    service: &S,
    workspace_id: &str,
    output: &mut impl Write,
) -> Result<(), MetricsError> {
    let phases = service
        .query_phases(PhasesQuery {
            workspace_id: workspace_id.to_owned(),
        })
        .map_err(MetricsError::QueryPhases)?
        .phases;
    if phases.is_empty() {
        writeln!(output, "no phases recorded for mission {workspace_id}")?;
        return Ok(());
    }

    writeln!(
        output,
        "{:<24}  {:<28}  {:<8}  {:<8}  parsed_skills",
        "phase", "persona", "status", "duration"
    )?;
    writeln!(output, "{}", "-".repeat(100))?;
    for phase in &phases {
        let skills = phase.parsed_skills.join(", ");
        writeln!(
            output,
            "{:<24}  {:<28}  {:<8}  {:<7}s  {skills}",
            truncate(&phase.name, 24),
            truncate(&phase.persona, 28),
            phase.status,
            phase.duration_s,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_app::{
        CapturingMetricsQueryService, DayTrend, MetricsQueryCall, MetricsQueryFixtureResponses,
        MissionSummary, MissionsResult, PersonaMetric, PersonaMetricsResult, PhaseRow,
        PhasesResult, RoutingMethodDist, RoutingMethodsResult, SkillUsage, SkillUsageResult,
        TrendsResult, UnenrolledMetricsQueryService,
    };

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    #[test]
    fn unenrolled_service_fails_closed_before_writing_output() {
        let service = UnenrolledMetricsQueryService;
        let mut output = Vec::new();
        let result = show_missions(&service, &MissionsFlags::default(), &mut output);

        assert!(matches!(
            result,
            Err(MetricsError::QueryMissions(
                MetricsQueryError::OwnerQueryNotEnrolled { .. }
            ))
        ));
        assert!(output.is_empty());
    }

    #[test]
    fn empty_storage_free_fixture_prints_no_missions_recorded() -> TestResult {
        let service = CapturingMetricsQueryService::default();

        let mut output = Vec::new();
        show_missions(&service, &MissionsFlags::default(), &mut output)?;
        assert_eq!(String::from_utf8(output)?, "no missions recorded yet\n");
        Ok(())
    }

    #[test]
    fn empty_trends_message_uses_raw_unclamped_days() -> TestResult {
        // Go's `showTrends` (metrics.go:238-244) prints the *raw* `--days`
        // flag value in the empty-result message, even though
        // `db.QueryTrends` clamps `days<=0` to 30 internally for the SQL
        // query itself. Regression test for that divergence.
        let service = CapturingMetricsQueryService::default();

        let mut output = Vec::new();
        show_trends(&service, 0, &mut output)?;

        assert_eq!(
            String::from_utf8(output)?,
            "no missions in the last 0 days\n"
        );
        Ok(())
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn storage_free_service_proves_all_forwarding_and_exact_formatting() -> TestResult {
        let service = CapturingMetricsQueryService::new(MetricsQueryFixtureResponses {
            missions: MissionsResult {
                missions: vec![MissionSummary {
                    workspace_id: "ws-1".to_owned(),
                    domain: "dev".to_owned(),
                    status: "success".to_owned(),
                    decomp_source: "llm".to_owned(),
                    task: "do thing".to_owned(),
                    top_persona: "architect".to_owned(),
                    duration_sec: 42,
                    phases_total: 2,
                    phases_completed: 2,
                    phases_failed: 0,
                    started_at: "2026-07-16T00:00:00Z".to_owned(),
                }],
            },
            personas: PersonaMetricsResult {
                personas: vec![PersonaMetric {
                    persona: "architect".to_owned(),
                    phase_count: 2,
                    avg_duration_sec: 12.4,
                    failure_rate: 25.0,
                    avg_retries: 0.5,
                    llm_pct: 75.0,
                    keyword_pct: 25.0,
                }],
            },
            skills: SkillUsageResult {
                skills: vec![SkillUsage {
                    skill_name: "rust-best-practices".to_owned(),
                    phase: "implement".to_owned(),
                    persona: "developer".to_owned(),
                    source: "declared".to_owned(),
                    invocations: 3,
                }],
            },
            trends: TrendsResult {
                trends: vec![DayTrend {
                    day: "2026-07-16".to_owned(),
                    total: 4,
                    successes: 3,
                    avg_duration: 15.0,
                }],
            },
            routing_methods: RoutingMethodsResult {
                methods: vec![
                    RoutingMethodDist {
                        method: "llm".to_owned(),
                        count: 6,
                        pct: 60.0,
                    },
                    RoutingMethodDist {
                        method: "fallback".to_owned(),
                        count: 4,
                        pct: 40.0,
                    },
                ],
            },
            phases: PhasesResult {
                phases: vec![PhaseRow {
                    name: "implement".to_owned(),
                    persona: "developer".to_owned(),
                    status: "success".to_owned(),
                    duration_s: 7,
                    parsed_skills: vec!["rust-best-practices".to_owned(), "testing".to_owned()],
                }],
            },
        });

        let mission_flags = MissionsFlags {
            last: 7,
            domain: "work".to_owned(),
            status: "failed".to_owned(),
            days: 14,
            decomp_source: "planner".to_owned(),
            worker: "glm".to_owned(),
        };
        let cases = [
            (
                MetricsCommand::Missions(mission_flags.clone()),
                concat!(
                    "workspace     domain      status    decomp          persona                 duration phases        task\n",
                    "------------------------------------------------------------------------------------------------------------------\n",
                    "ws-1          dev         success   llm             architect               42s      2/2           do thing\n",
                    "\n1 missions  •  1 succeeded  •  0 failed  •  avg 42s\n",
                ),
            ),
            (
                MetricsCommand::Personas,
                concat!(
                    "persona                         phases   avg_dur   fail%  avg_retry   llm%    kw%\n",
                    "--------------------------------------------------------------------------------\n",
                    "architect                            2       12s   25.0%       0.50    75%    25%\n",
                ),
            ),
            (
                MetricsCommand::Skills,
                concat!(
                    "skill                         phase                 persona                   source        uses\n",
                    "------------------------------------------------------------------------------------------------\n",
                    "rust-best-practices           implement             developer                 declared      3\n",
                ),
            ),
            (
                MetricsCommand::Trends { days: 9 },
                concat!(
                    "day         missions   success%   avg_dur\n",
                    "------------------------------------------\n",
                    "2026-07-16         4      75.0%       15s\n",
                    "\n1 days  •  4 missions  •  75.0% success  •  avg 15s\n",
                ),
            ),
            (
                MetricsCommand::RoutingMethods,
                concat!(
                    "method                  phases      pct%\n",
                    "------------------------------------------\n",
                    "llm                          6     60.0%\n",
                    "fallback                     4     40.0%\n",
                    "\n10 phases total\n",
                    "\nALERT: fallback routing rate 40.0% exceeds 30% threshold — LLM decomposition may be failing too often\n",
                ),
            ),
            (
                MetricsCommand::Phases {
                    workspace_id: "ws-77".to_owned(),
                },
                concat!(
                    "phase                     persona                       status    duration  parsed_skills\n",
                    "----------------------------------------------------------------------------------------------------\n",
                    "implement                 developer                     success   7      s  rust-best-practices, testing\n",
                ),
            ),
        ];

        for (command, expected) in cases {
            let mut output = Vec::new();
            run(&service, &command, &mut output)?;
            assert_eq!(std::str::from_utf8(&output)?, expected);
        }

        assert_eq!(
            service.recorded_calls(),
            vec![
                MetricsQueryCall::Missions(MissionsQuery {
                    limit: 7,
                    domain: "work".to_owned(),
                    days: 14,
                    status: "failed".to_owned(),
                    decomp_source: "planner".to_owned(),
                    worker: "glm".to_owned(),
                }),
                MetricsQueryCall::PersonaMetrics(PersonaMetricsQuery),
                MetricsQueryCall::SkillUsage(SkillUsageQuery),
                MetricsQueryCall::Trends(TrendsQuery { days: 9 }),
                MetricsQueryCall::RoutingMethods(RoutingMethodsQuery),
                MetricsQueryCall::Phases(PhasesQuery {
                    workspace_id: "ws-77".to_owned(),
                }),
            ]
        );
        Ok(())
    }

    #[test]
    fn routing_subcommand_is_flagged_not_ported() {
        let service = UnenrolledMetricsQueryService;
        let mut output = Vec::new();
        let result = run(&service, &MetricsCommand::Routing, &mut output);
        assert!(matches!(result, Err(MetricsError::RoutingReaderNotPorted)));
    }
}
