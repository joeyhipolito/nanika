//! Typed application boundary for metrics queries.
//!
//! A [`MetricsQueryService`] receives already-typed requests and returns
//! already-typed rows. It does not expose a SQLite connection or accept a
//! filesystem path. Production callers must obtain results from the current
//! storage owner. The borrowed connection adapter and raw SQL parity helpers
//! compile only in this crate's tests, so production code cannot turn an
//! arbitrary pathname into a first-party storage capability.

#[cfg(any(test, feature = "test-support"))]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;
use thiserror::Error;

use crate::metrics_read::{
    DayTrend, MetricsReadError, MissionSummary, PersonaMetric, PhaseRow, RoutingMethodDist,
    SkillUsage,
};
#[cfg(test)]
use crate::metrics_read::{
    query_missions, query_persona_metrics, query_phases, query_routing_method_distribution,
    query_skill_usage, query_trends,
};

/// Version reserved for the future owner-mediated metrics query protocol.
pub const METRICS_OWNER_QUERY_PROTOCOL: &str = "metrics-query-v1";

/// Filters for the top-level mission metrics query.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct MissionsQuery {
    /// Maximum number of mission rows. Non-positive values retain Go's
    /// default of 20.
    pub limit: i64,
    /// Exact domain filter, or empty for all domains.
    pub domain: String,
    /// Only include missions from this many recent days; zero disables the
    /// filter.
    pub days: i64,
    /// Exact mission-status filter, or empty for all statuses.
    pub status: String,
    /// Exact decomposition-source filter, or empty for every source.
    pub decomp_source: String,
    /// Exact worker filter, or empty for every worker.
    pub worker: String,
}

impl MissionsQuery {
    /// Clamps `limit` into `1..=`[`crate::MAX_QUERY_LIMIT`], keeping Go's
    /// default of 20 for a non-positive value.
    ///
    /// This is a caller convenience, not the enforcement point: a service
    /// implementation is free to skip it, and only the two `cfg(test)`
    /// adapters in this module call it. Boundedness is enforced where the SQL
    /// is built: the `metrics_read::query_*` reader behind this request,
    /// `query_missions`, clamps its own `limit` argument through the same
    /// `metrics_owner::clamp_limit`, so no caller-supplied number reaches
    /// storage unclamped whether or not this method was called.
    #[must_use]
    pub fn bounded(mut self) -> Self {
        self.limit = crate::metrics_owner::clamp_limit(
            self.limit,
            crate::metrics_owner::DEFAULT_MISSIONS_LIMIT,
        );
        self
    }
}

/// Typed result for [`MetricsQueryService::query_missions`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MissionsResult {
    /// Mission summaries in the storage owner's query order.
    pub missions: Vec<MissionSummary>,
}

/// Request for aggregate persona metrics.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct PersonaMetricsQuery;

/// Typed result for [`MetricsQueryService::query_persona_metrics`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PersonaMetricsResult {
    /// Aggregate persona rows in the storage owner's query order.
    pub personas: Vec<PersonaMetric>,
}

/// Request for aggregate skill usage.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct SkillUsageQuery;

/// Typed result for [`MetricsQueryService::query_skill_usage`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SkillUsageResult {
    /// Aggregate skill-usage rows in the storage owner's query order.
    pub skills: Vec<SkillUsage>,
}

/// Filters for daily mission trends.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TrendsQuery {
    /// Number of recent days to aggregate. Non-positive values retain Go's
    /// query default of 30.
    pub days: i64,
}

impl TrendsQuery {
    /// Clamps `days` into `1..=`[`crate::MAX_QUERY_LIMIT`], keeping Go's query
    /// default of 30 for a non-positive value.
    #[must_use]
    pub const fn bounded(mut self) -> Self {
        self.days = crate::metrics_owner::clamp_limit(self.days, DEFAULT_TRENDS_DAYS);
        self
    }
}

/// Go's default trend window for a non-positive `days` (`db.go:1337-1366`).
pub const DEFAULT_TRENDS_DAYS: i64 = 30;

/// Typed result for [`MetricsQueryService::query_trends`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrendsResult {
    /// Daily trend rows in reverse chronological order.
    pub trends: Vec<DayTrend>,
}

/// Request for routing-method distribution.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct RoutingMethodsQuery;

/// Typed result for [`MetricsQueryService::query_routing_methods`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoutingMethodsResult {
    /// Routing-method distribution rows ordered by count.
    pub methods: Vec<RoutingMethodDist>,
}

/// Selects phase rows for one mission workspace.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PhasesQuery {
    /// Mission/workspace identifier whose phase rows should be returned.
    pub workspace_id: String,
}

/// Typed result for [`MetricsQueryService::query_phases`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhasesResult {
    /// Phase rows in their recorded order.
    pub phases: Vec<PhaseRow>,
}

/// Bounded request used by the retained metrics owner to publish one private run.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PrivateRunMetricsQuery {
    /// Maximum number of phase rows admitted to the projection.
    pub limit: usize,
}

/// Mission aggregate read through the retained owner's SQLite connection.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq)]
pub struct PrivateMissionMetricRow {
    pub status: String,
    pub duration_s: i64,
    pub phases_total: i64,
    pub phases_completed: i64,
    pub phases_failed: i64,
    pub phases_skipped: i64,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub tokens_cache_creation: i64,
    pub tokens_cache_read: i64,
    pub tokens_known: bool,
    pub cost_usd: f64,
    pub cost_known: bool,
}

/// Phase row read through the retained owner's SQLite connection.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq)]
pub struct PrivatePhaseMetricRow {
    pub phase: String,
    pub status: String,
    pub persona: String,
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub duration_s: i64,
    pub gate_passed: bool,
    pub target_released: bool,
    pub parsed_skills: Vec<String>,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub tokens_cache_creation: i64,
    pub tokens_cache_read: i64,
    pub tokens_known: bool,
    pub cost_usd: f64,
    pub cost_known: bool,
}

/// Complete owner-mediated source for one private run projection.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq)]
pub struct PrivateRunMetricsResult {
    pub mission_id: String,
    pub mission_finished: bool,
    pub mission: Option<PrivateMissionMetricRow>,
    pub phases: Vec<PrivatePhaseMetricRow>,
}

/// Failures at the metrics query authority boundary.
#[derive(Debug, Error)]
pub enum MetricsQueryError {
    /// The authority-owned or private-snapshot connection rejected the query.
    #[error(transparent)]
    Read(#[from] MetricsReadError),
    /// The owner connection contained data that cannot form a safe private run.
    #[error("owner metrics cannot form a bounded private run: {reason}")]
    PrivateRunRefused {
        /// Stable structural reason that carries no stored content.
        reason: &'static str,
    },
    /// Stage A deliberately has no ambient-path fallback. The active owner
    /// protocol or a cooperative cross-language lease must land first.
    #[error(
        "metrics owner-query protocol {protocol} is not enrolled; direct metrics.db access \
         is disabled until Go and Rust share a cooperative writer lock; use the Go rollback \
         command for metrics until one of those authority paths is enrolled"
    )]
    OwnerQueryNotEnrolled {
        /// Owner-query protocol required before live metrics reads can be
        /// served safely.
        protocol: &'static str,
    },
}

/// Authority-preserving metrics query surface.
///
/// Implementations may query a connection already owned by the storage actor,
/// call a future authenticated owner protocol, or inspect a private snapshot.
/// Implementations must not discover and open a live `metrics.db` by ambient
/// pathname. The trait is sealed: implementations are supplied by this crate,
/// so a downstream caller cannot relabel an ambient SQLite connection as
/// owner-authorized.
pub trait MetricsQueryService: sealed::Sealed {
    /// Queries mission summaries using the supplied filters.
    fn query_missions(&self, query: MissionsQuery) -> Result<MissionsResult, MetricsQueryError>;

    /// Queries aggregate persona metrics.
    fn query_persona_metrics(
        &self,
        query: PersonaMetricsQuery,
    ) -> Result<PersonaMetricsResult, MetricsQueryError>;

    /// Queries aggregate skill usage.
    fn query_skill_usage(
        &self,
        query: SkillUsageQuery,
    ) -> Result<SkillUsageResult, MetricsQueryError>;

    /// Queries daily mission trends.
    fn query_trends(&self, query: TrendsQuery) -> Result<TrendsResult, MetricsQueryError>;

    /// Queries the routing-method distribution.
    fn query_routing_methods(
        &self,
        query: RoutingMethodsQuery,
    ) -> Result<RoutingMethodsResult, MetricsQueryError>;

    /// Queries recorded phases for one mission/workspace.
    fn query_phases(&self, query: PhasesQuery) -> Result<PhasesResult, MetricsQueryError>;

    /// Queries the single bounded run used for an owner-published projection.
    #[doc(hidden)]
    fn query_private_run(
        &self,
        _query: PrivateRunMetricsQuery,
    ) -> Result<Option<PrivateRunMetricsResult>, MetricsQueryError> {
        Err(MetricsQueryError::OwnerQueryNotEnrolled {
            protocol: METRICS_OWNER_QUERY_PROTOCOL,
        })
    }
}

const MAX_PRIVATE_TEXT_BYTES: usize = 256;
const MAX_PRIVATE_TIMESTAMP_BYTES: usize = 64;
const MAX_PRIVATE_SKILLS_JSON_BYTES: usize = 4 * 1024;
const MAX_PRIVATE_SKILLS: usize = 128;
const MAX_PRIVATE_COUNTER: i64 = 9_007_199_254_740_991;
const MAX_PRIVATE_COST_USD: f64 = 1_000_000_000_000.0;

pub(crate) fn query_private_run_on_connection(
    connection: &Connection,
    query: PrivateRunMetricsQuery,
) -> Result<Option<PrivateRunMetricsResult>, MetricsQueryError> {
    if query.limit == 0 || query.limit > crate::metrics_owner::MAX_QUERY_LIMIT {
        return refused("invalid phase limit");
    }
    validate_private_schema(connection)?;

    let malformed_mission_text: i64 = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM missions
                WHERE length(id) > ?1 OR length(started_at) > ?2
                   OR length(finished_at) > ?2 OR length(status) > ?1
                LIMIT 1
             )",
            rusqlite::params![
                MAX_PRIVATE_TEXT_BYTES as i64,
                MAX_PRIVATE_TIMESTAMP_BYTES as i64
            ],
            |row| row.get(0),
        )
        .map_err(|source| private_query_error("private mission text bounds", source))?;
    if malformed_mission_text != 0 {
        return refused("mission text exceeds its bound");
    }

    let mut mission_statement = connection
        .prepare(
            "SELECT id, started_at, finished_at, status, duration_s, phases_total,
                    phases_completed, phases_failed, phases_skipped, tokens_in_total,
                    tokens_out_total, tokens_cache_creation_total, tokens_cache_read_total,
                    tokens_known, cost_usd_total, cost_known
             FROM missions ORDER BY rowid LIMIT 2",
        )
        .map_err(|source| private_query_error("prepare private mission", source))?;
    let mission_rows = mission_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
                row.get::<_, i64>(12)?,
                row.get::<_, i64>(13)?,
                row.get::<_, f64>(14)?,
                row.get::<_, i64>(15)?,
            ))
        })
        .map_err(|source| private_query_error("query private mission", source))?;
    let mut missions = Vec::with_capacity(2);
    for row in mission_rows {
        missions.push(row.map_err(|source| private_query_error("decode private mission", source))?);
    }
    if missions.len() > 1 {
        return refused("multiple mission identities");
    }
    let Some((
        mission_id,
        started_at,
        finished_at,
        status,
        duration_s,
        phases_total,
        phases_completed,
        phases_failed,
        phases_skipped,
        tokens_in,
        tokens_out,
        tokens_cache_creation,
        tokens_cache_read,
        tokens_known,
        cost_usd,
        cost_known,
    )) = missions.pop()
    else {
        let has_phases: i64 = connection
            .query_row("SELECT EXISTS(SELECT 1 FROM phases LIMIT 1)", [], |row| {
                row.get(0)
            })
            .map_err(|source| private_query_error("check orphan private phases", source))?;
        return if has_phases == 0 {
            Ok(None)
        } else {
            refused("phase rows exist without a mission")
        };
    };
    if mission_id.len() > MAX_PRIVATE_TEXT_BYTES
        || orchestrator_core::MissionId::new(mission_id.clone()).is_err()
    {
        return refused("invalid mission identity");
    }

    let malformed_phase_text: i64 = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM phases
                WHERE length(id) > ?1 OR length(mission_id) > ?1 OR length(name) > ?1
                   OR length(status) > ?1 OR length(persona) > ?1 OR length(provider) > ?1
                   OR length(model) > ?1 OR length(effort) > ?1 OR length(parsed_skills) > ?2
                LIMIT 1
             )",
            rusqlite::params![
                MAX_PRIVATE_TEXT_BYTES as i64,
                MAX_PRIVATE_SKILLS_JSON_BYTES as i64
            ],
            |row| row.get(0),
        )
        .map_err(|source| private_query_error("private phase text bounds", source))?;
    if malformed_phase_text != 0 {
        return refused("phase text exceeds its bound");
    }

    let probe = i64::try_from(query.limit)
        .unwrap_or(i64::MAX)
        .saturating_add(1);
    let mut identity_statement = connection
        .prepare("SELECT mission_id FROM phases ORDER BY rowid LIMIT ?1")
        .map_err(|source| private_query_error("prepare private phase identities", source))?;
    let identity_rows = identity_statement
        .query_map([probe], |row| row.get::<_, String>(0))
        .map_err(|source| private_query_error("query private phase identities", source))?;
    let mut phase_count = 0usize;
    for row in identity_rows {
        let identity =
            row.map_err(|source| private_query_error("decode private phase identity", source))?;
        phase_count = phase_count.saturating_add(1);
        if identity != mission_id {
            return refused("foreign phase mission identity");
        }
    }
    if phase_count > query.limit {
        return refused("too many phase rows");
    }

    let mut phase_statement = connection
        .prepare(
            "SELECT id, name, status, persona, provider, model, effort, duration_s,
                    gate_passed, target_released, parsed_skills, tokens_in, tokens_out,
                    tokens_cache_creation, tokens_cache_read, tokens_known, cost_usd, cost_known
             FROM phases WHERE mission_id = ?1 ORDER BY rowid LIMIT ?2",
        )
        .map_err(|source| private_query_error("prepare private phases", source))?;
    let rows = phase_statement
        .query_map(rusqlite::params![mission_id, probe], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, i64>(11)?,
                row.get::<_, i64>(12)?,
                row.get::<_, i64>(13)?,
                row.get::<_, i64>(14)?,
                row.get::<_, i64>(15)?,
                row.get::<_, f64>(16)?,
                row.get::<_, i64>(17)?,
            ))
        })
        .map_err(|source| private_query_error("query private phases", source))?;
    let mut phases = Vec::with_capacity(phase_count);
    let mut phase_names = BTreeSet::new();
    for row in rows {
        let (
            row_id,
            phase,
            phase_status,
            persona,
            provider,
            model,
            effort,
            phase_duration_s,
            gate_passed,
            target_released,
            skills_json,
            phase_tokens_in,
            phase_tokens_out,
            phase_tokens_cache_creation,
            phase_tokens_cache_read,
            phase_tokens_known,
            phase_cost_usd,
            phase_cost_known,
        ) = row.map_err(|source| private_query_error("decode private phase", source))?;
        if row_id != format!("{mission_id}_{phase}") || !phase_names.insert(phase.clone()) {
            return refused("invalid or duplicate phase identity");
        }
        if phase.is_empty()
            || !valid_private_text(&phase)
            || !valid_private_text(&phase_status)
            || !matches!(phase_status.as_str(), "completed" | "failed" | "skipped")
            || !valid_private_text_allow_empty(&persona)
            || !valid_private_text_allow_empty(&provider)
            || !valid_private_text_allow_empty(&model)
            || !valid_private_text_allow_empty(&effort)
            || !valid_counter(phase_duration_s)
            || !valid_boolean(gate_passed)
            || !valid_boolean(target_released)
            || !valid_boolean(phase_tokens_known)
            || !valid_boolean(phase_cost_known)
            || !valid_counter(phase_tokens_in)
            || !valid_counter(phase_tokens_out)
            || !valid_counter(phase_tokens_cache_creation)
            || !valid_counter(phase_tokens_cache_read)
            || !valid_cost(phase_cost_usd)
        {
            return refused("malformed phase row");
        }
        let parsed_skills = parse_private_skills(&skills_json)?;
        phases.push(PrivatePhaseMetricRow {
            phase,
            status: phase_status,
            persona,
            provider,
            model,
            effort,
            duration_s: phase_duration_s,
            gate_passed: gate_passed == 1,
            target_released: target_released == 1,
            parsed_skills,
            tokens_in: phase_tokens_in,
            tokens_out: phase_tokens_out,
            tokens_cache_creation: phase_tokens_cache_creation,
            tokens_cache_read: phase_tokens_cache_read,
            tokens_known: phase_tokens_known == 1,
            cost_usd: phase_cost_usd,
            cost_known: phase_cost_known == 1,
        });
    }
    if phases.len() != phase_count {
        return refused("phase identity scan changed during query");
    }

    let mission_finished = !finished_at.is_empty();
    if (!started_at.is_empty() && !valid_private_timestamp(&started_at))
        || (mission_finished && !valid_private_timestamp(&finished_at))
        || !valid_private_text(&status)
        || !valid_counter(duration_s)
        || !valid_counter(phases_total)
        || !valid_counter(phases_completed)
        || !valid_counter(phases_failed)
        || !valid_counter(phases_skipped)
        || !valid_counter(tokens_in)
        || !valid_counter(tokens_out)
        || !valid_counter(tokens_cache_creation)
        || !valid_counter(tokens_cache_read)
        || !valid_boolean(tokens_known)
        || !valid_boolean(cost_known)
        || !valid_cost(cost_usd)
    {
        return refused("malformed mission row");
    }
    let phase_total = i64::try_from(phases.len()).unwrap_or(i64::MAX);
    if mission_finished
        && (phases_total != phase_total
            || phases_completed
                .saturating_add(phases_failed)
                .saturating_add(phases_skipped)
                != phases_total)
    {
        return refused("mission aggregates disagree with phase rows");
    }
    let mission = mission_finished.then_some(PrivateMissionMetricRow {
        status,
        duration_s,
        phases_total,
        phases_completed,
        phases_failed,
        phases_skipped,
        tokens_in,
        tokens_out,
        tokens_cache_creation,
        tokens_cache_read,
        tokens_known: tokens_known == 1,
        cost_usd,
        cost_known: cost_known == 1,
    });
    Ok(Some(PrivateRunMetricsResult {
        mission_id,
        mission_finished,
        mission,
        phases,
    }))
}

fn validate_private_schema(connection: &Connection) -> Result<(), MetricsQueryError> {
    validate_private_table(
        connection,
        "missions",
        &[
            ("id", "TEXT"),
            ("started_at", "DATETIME"),
            ("finished_at", "DATETIME"),
            ("status", "TEXT"),
            ("duration_s", "INTEGER"),
            ("phases_total", "INTEGER"),
            ("phases_completed", "INTEGER"),
            ("phases_failed", "INTEGER"),
            ("phases_skipped", "INTEGER"),
            ("tokens_in_total", "INTEGER"),
            ("tokens_out_total", "INTEGER"),
            ("tokens_cache_creation_total", "INTEGER"),
            ("tokens_cache_read_total", "INTEGER"),
            ("tokens_known", "INTEGER"),
            ("cost_usd_total", "REAL"),
            ("cost_known", "INTEGER"),
        ],
    )?;
    validate_private_table(
        connection,
        "phases",
        &[
            ("id", "TEXT"),
            ("mission_id", "TEXT"),
            ("name", "TEXT"),
            ("status", "TEXT"),
            ("persona", "TEXT"),
            ("provider", "TEXT"),
            ("model", "TEXT"),
            ("effort", "TEXT"),
            ("duration_s", "INTEGER"),
            ("gate_passed", "INTEGER"),
            ("target_released", "INTEGER"),
            ("parsed_skills", "TEXT"),
            ("tokens_in", "INTEGER"),
            ("tokens_out", "INTEGER"),
            ("tokens_cache_creation", "INTEGER"),
            ("tokens_cache_read", "INTEGER"),
            ("tokens_known", "INTEGER"),
            ("cost_usd", "REAL"),
            ("cost_known", "INTEGER"),
        ],
    )
}

fn validate_private_table(
    connection: &Connection,
    table: &'static str,
    required: &[(&'static str, &'static str)],
) -> Result<(), MetricsQueryError> {
    let sql = match table {
        "missions" => {
            "SELECT substr(name, 1, 65), length(name), substr(upper(type), 1, 33), length(type)
             FROM pragma_table_info('missions') LIMIT 65"
        }
        "phases" => {
            "SELECT substr(name, 1, 65), length(name), substr(upper(type), 1, 33), length(type)
             FROM pragma_table_info('phases') LIMIT 65"
        }
        _ => return refused("unknown private schema table"),
    };
    let mut statement = connection
        .prepare(sql)
        .map_err(|source| private_query_error("prepare private schema", source))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|source| private_query_error("query private schema", source))?;
    let mut columns = BTreeMap::new();
    for row in rows {
        let (name, name_length, kind, kind_length) =
            row.map_err(|source| private_query_error("decode private schema", source))?;
        if !(0..=64).contains(&name_length) || !(0..=32).contains(&kind_length) {
            return refused("schema identifier exceeds its bound");
        }
        columns.insert(name, kind);
    }
    if required
        .iter()
        .any(|(name, kind)| columns.get(*name).map(String::as_str) != Some(*kind))
    {
        return refused("required metrics schema is absent or incompatible");
    }
    Ok(())
}

fn parse_private_skills(value: &str) -> Result<Vec<String>, MetricsQueryError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let skills: Vec<String> =
        serde_json::from_str(value).map_err(|_| MetricsQueryError::PrivateRunRefused {
            reason: "malformed parsed skills",
        })?;
    if skills.len() > MAX_PRIVATE_SKILLS || skills.iter().any(|skill| !valid_private_text(skill)) {
        return refused("parsed skills exceed their bound");
    }
    Ok(skills)
}

fn valid_private_text(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PRIVATE_TEXT_BYTES
        && !value.chars().any(char::is_control)
}

fn valid_private_text_allow_empty(value: &str) -> bool {
    value.len() <= MAX_PRIVATE_TEXT_BYTES && !value.chars().any(char::is_control)
}

fn valid_private_timestamp(value: &str) -> bool {
    if value.len() < 20 || value.len() > MAX_PRIVATE_TIMESTAMP_BYTES || !value.is_ascii() {
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
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        number(0..4),
        number(5..7),
        number(8..10),
        number(11..13),
        number(14..16),
        number(17..19),
    ) else {
        return false;
    };
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
    match bytes.get(19) {
        Some(b'Z') => bytes.len() == 20,
        Some(b'.') => {
            bytes.len() > 21
                && bytes.last() == Some(&b'Z')
                && bytes[20..bytes.len() - 1].iter().all(u8::is_ascii_digit)
        }
        _ => false,
    }
}

const fn valid_boolean(value: i64) -> bool {
    value == 0 || value == 1
}

const fn valid_counter(value: i64) -> bool {
    value >= 0 && value <= MAX_PRIVATE_COUNTER
}

fn valid_cost(value: f64) -> bool {
    value.is_finite() && (0.0..=MAX_PRIVATE_COST_USD).contains(&value)
}

fn private_query_error(what: &'static str, source: rusqlite::Error) -> MetricsQueryError {
    MetricsReadError::Query { what, source }.into()
}

fn refused<T>(reason: &'static str) -> Result<T, MetricsQueryError> {
    Err(MetricsQueryError::PrivateRunRefused { reason })
}

/// Test-only adapter for an in-memory or isolated fixture connection.
///
/// This type never opens a database, cannot return its borrowed connection,
/// and is not present in production builds.
#[cfg(test)]
pub(crate) struct ConnectionMetricsQueryService<'connection> {
    connection: &'connection Connection,
}

#[cfg(test)]
impl<'connection> ConnectionMetricsQueryService<'connection> {
    /// Borrows an already-authorized connection without opening storage.
    #[must_use]
    pub(crate) const fn new(connection: &'connection Connection) -> Self {
        Self { connection }
    }
}

#[cfg(test)]
impl sealed::Sealed for ConnectionMetricsQueryService<'_> {}

#[cfg(test)]
impl MetricsQueryService for ConnectionMetricsQueryService<'_> {
    fn query_missions(&self, query: MissionsQuery) -> Result<MissionsResult, MetricsQueryError> {
        let query = query.bounded();
        let missions = query_missions(
            self.connection,
            query.limit,
            &query.domain,
            query.days,
            &query.status,
            &query.decomp_source,
            &query.worker,
        )?;
        Ok(MissionsResult { missions })
    }

    fn query_persona_metrics(
        &self,
        _query: PersonaMetricsQuery,
    ) -> Result<PersonaMetricsResult, MetricsQueryError> {
        let personas = query_persona_metrics(self.connection)?;
        Ok(PersonaMetricsResult { personas })
    }

    fn query_skill_usage(
        &self,
        _query: SkillUsageQuery,
    ) -> Result<SkillUsageResult, MetricsQueryError> {
        let skills = query_skill_usage(self.connection)?;
        Ok(SkillUsageResult { skills })
    }

    fn query_trends(&self, query: TrendsQuery) -> Result<TrendsResult, MetricsQueryError> {
        let query = query.bounded();
        let trends = query_trends(self.connection, query.days)?;
        Ok(TrendsResult { trends })
    }

    fn query_routing_methods(
        &self,
        _query: RoutingMethodsQuery,
    ) -> Result<RoutingMethodsResult, MetricsQueryError> {
        let methods = query_routing_method_distribution(self.connection)?;
        Ok(RoutingMethodsResult { methods })
    }

    fn query_phases(&self, query: PhasesQuery) -> Result<PhasesResult, MetricsQueryError> {
        let phases = query_phases(self.connection, &query.workspace_id)?;
        Ok(PhasesResult { phases })
    }
}

/// Typed responses supplied by [`CapturingMetricsQueryService`].
///
/// This test-support value contains no path, connection, or storage-opening
/// behavior. It exists only when the `test-support` feature (or this crate's
/// own tests) is enabled.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MetricsQueryFixtureResponses {
    /// Mission-query response.
    pub missions: MissionsResult,
    /// Persona-query response.
    pub personas: PersonaMetricsResult,
    /// Skill-query response.
    pub skills: SkillUsageResult,
    /// Trend-query response.
    pub trends: TrendsResult,
    /// Routing-method-query response.
    pub routing_methods: RoutingMethodsResult,
    /// Phase-query response.
    pub phases: PhasesResult,
}

/// One typed invocation captured by [`CapturingMetricsQueryService`].
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum MetricsQueryCall {
    /// Mission summaries were requested with these filters.
    Missions(MissionsQuery),
    /// Aggregate persona metrics were requested.
    PersonaMetrics(PersonaMetricsQuery),
    /// Aggregate skill usage was requested.
    SkillUsage(SkillUsageQuery),
    /// Daily trends were requested with these filters.
    Trends(TrendsQuery),
    /// Routing-method distribution was requested.
    RoutingMethods(RoutingMethodsQuery),
    /// Mission phase rows were requested with these filters.
    Phases(PhasesQuery),
}

/// Storage-free query service for downstream formatting and forwarding tests.
///
/// Each method records its typed request and returns a clone of the configured
/// typed response. It has no filesystem or SQLite capability, so enabling test
/// support cannot bypass production storage ownership.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct CapturingMetricsQueryService {
    responses: MetricsQueryFixtureResponses,
    calls: RefCell<Vec<MetricsQueryCall>>,
}

#[cfg(any(test, feature = "test-support"))]
impl CapturingMetricsQueryService {
    /// Creates a storage-free service with predetermined typed responses.
    #[must_use]
    pub fn new(responses: MetricsQueryFixtureResponses) -> Self {
        Self {
            responses,
            calls: RefCell::new(Vec::new()),
        }
    }

    /// Returns the calls recorded so far in invocation order.
    #[must_use]
    pub fn recorded_calls(&self) -> Vec<MetricsQueryCall> {
        self.calls.borrow().clone()
    }

    fn record(&self, call: MetricsQueryCall) {
        self.calls.borrow_mut().push(call);
    }
}

#[cfg(any(test, feature = "test-support"))]
impl sealed::Sealed for CapturingMetricsQueryService {}

#[cfg(any(test, feature = "test-support"))]
impl MetricsQueryService for CapturingMetricsQueryService {
    fn query_missions(&self, query: MissionsQuery) -> Result<MissionsResult, MetricsQueryError> {
        self.record(MetricsQueryCall::Missions(query));
        Ok(self.responses.missions.clone())
    }

    fn query_persona_metrics(
        &self,
        query: PersonaMetricsQuery,
    ) -> Result<PersonaMetricsResult, MetricsQueryError> {
        self.record(MetricsQueryCall::PersonaMetrics(query));
        Ok(self.responses.personas.clone())
    }

    fn query_skill_usage(
        &self,
        query: SkillUsageQuery,
    ) -> Result<SkillUsageResult, MetricsQueryError> {
        self.record(MetricsQueryCall::SkillUsage(query));
        Ok(self.responses.skills.clone())
    }

    fn query_trends(&self, query: TrendsQuery) -> Result<TrendsResult, MetricsQueryError> {
        self.record(MetricsQueryCall::Trends(query));
        Ok(self.responses.trends.clone())
    }

    fn query_routing_methods(
        &self,
        query: RoutingMethodsQuery,
    ) -> Result<RoutingMethodsResult, MetricsQueryError> {
        self.record(MetricsQueryCall::RoutingMethods(query));
        Ok(self.responses.routing_methods.clone())
    }

    fn query_phases(&self, query: PhasesQuery) -> Result<PhasesResult, MetricsQueryError> {
        self.record(MetricsQueryCall::Phases(query));
        Ok(self.responses.phases.clone())
    }
}

/// Fail-closed production service used until an owner query protocol or a
/// cooperative read lease is enrolled.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnenrolledMetricsQueryService;

impl UnenrolledMetricsQueryService {
    fn unavailable<T>() -> Result<T, MetricsQueryError> {
        Err(MetricsQueryError::OwnerQueryNotEnrolled {
            protocol: METRICS_OWNER_QUERY_PROTOCOL,
        })
    }
}

impl sealed::Sealed for UnenrolledMetricsQueryService {}

impl MetricsQueryService for UnenrolledMetricsQueryService {
    fn query_missions(&self, _query: MissionsQuery) -> Result<MissionsResult, MetricsQueryError> {
        Self::unavailable()
    }

    fn query_persona_metrics(
        &self,
        _query: PersonaMetricsQuery,
    ) -> Result<PersonaMetricsResult, MetricsQueryError> {
        Self::unavailable()
    }

    fn query_skill_usage(
        &self,
        _query: SkillUsageQuery,
    ) -> Result<SkillUsageResult, MetricsQueryError> {
        Self::unavailable()
    }

    fn query_trends(&self, _query: TrendsQuery) -> Result<TrendsResult, MetricsQueryError> {
        Self::unavailable()
    }

    fn query_routing_methods(
        &self,
        _query: RoutingMethodsQuery,
    ) -> Result<RoutingMethodsResult, MetricsQueryError> {
        Self::unavailable()
    }

    fn query_phases(&self, _query: PhasesQuery) -> Result<PhasesResult, MetricsQueryError> {
        Self::unavailable()
    }
}

pub(crate) mod sealed {
    pub trait Sealed {}
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn seed_schema(connection: &Connection) -> rusqlite::Result<()> {
        connection.execute_batch(
            r"
            CREATE TABLE missions (
                id TEXT PRIMARY KEY,
                domain TEXT NOT NULL DEFAULT '',
                task TEXT NOT NULL DEFAULT '',
                started_at DATETIME NOT NULL,
                status TEXT NOT NULL DEFAULT '',
                decomp_source TEXT NOT NULL DEFAULT 'unknown',
                duration_s INTEGER NOT NULL DEFAULT 0,
                phases_total INTEGER NOT NULL DEFAULT 0,
                phases_completed INTEGER NOT NULL DEFAULT 0,
                phases_failed INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE phases (
                id TEXT PRIMARY KEY,
                mission_id TEXT NOT NULL,
                name TEXT NOT NULL DEFAULT '',
                persona TEXT NOT NULL DEFAULT '',
                selection_method TEXT NOT NULL DEFAULT '',
                duration_s INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL DEFAULT '',
                retries INTEGER NOT NULL DEFAULT 0,
                parsed_skills TEXT NOT NULL DEFAULT '',
                worker_name TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE skill_invocations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                mission_id TEXT NOT NULL,
                phase TEXT NOT NULL DEFAULT '',
                persona TEXT NOT NULL DEFAULT '',
                skill_name TEXT NOT NULL DEFAULT '',
                source TEXT NOT NULL DEFAULT 'declared'
            );
            ",
        )
    }

    #[test]
    fn borrowed_connection_service_returns_typed_rows() -> TestResult {
        let connection = Connection::open_in_memory()?;
        seed_schema(&connection)?;
        connection.execute(
            "INSERT INTO missions (id, domain, task, started_at, status, duration_s, \
             phases_total, phases_completed, phases_failed) VALUES \
             ('ws-1', 'dev', 'inspect', '2000-01-01T00:00:00Z', 'success', 12, 1, 1, 0)",
            [],
        )?;
        let service = ConnectionMetricsQueryService::new(&connection);

        let result = service.query_missions(MissionsQuery {
            limit: 20,
            ..MissionsQuery::default()
        })?;

        assert_eq!(result.missions.len(), 1);
        assert_eq!(result.missions[0].workspace_id, "ws-1");
        assert!(
            service
                .query_persona_metrics(PersonaMetricsQuery)?
                .personas
                .is_empty()
        );
        assert!(
            service
                .query_skill_usage(SkillUsageQuery)?
                .skills
                .is_empty()
        );
        assert!(
            service
                .query_trends(TrendsQuery { days: 1 })?
                .trends
                .is_empty()
        );
        assert!(
            service
                .query_routing_methods(RoutingMethodsQuery)?
                .methods
                .is_empty()
        );
        assert!(
            service
                .query_phases(PhasesQuery {
                    workspace_id: "ws-1".to_owned(),
                })?
                .phases
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn unenrolled_service_fails_closed_for_every_query() {
        let service = UnenrolledMetricsQueryService;

        let result = service.query_missions(MissionsQuery::default());
        assert!(matches!(
            &result,
            Err(MetricsQueryError::OwnerQueryNotEnrolled { .. })
        ));
        let guidance = result
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_default();
        assert!(guidance.contains(METRICS_OWNER_QUERY_PROTOCOL));
        assert!(guidance.contains("cooperative writer lock"));
        assert!(matches!(
            service.query_persona_metrics(PersonaMetricsQuery),
            Err(MetricsQueryError::OwnerQueryNotEnrolled { .. })
        ));
        assert!(matches!(
            service.query_skill_usage(SkillUsageQuery),
            Err(MetricsQueryError::OwnerQueryNotEnrolled { .. })
        ));
        assert!(matches!(
            service.query_trends(TrendsQuery { days: 30 }),
            Err(MetricsQueryError::OwnerQueryNotEnrolled { .. })
        ));
        assert!(matches!(
            service.query_routing_methods(RoutingMethodsQuery),
            Err(MetricsQueryError::OwnerQueryNotEnrolled { .. })
        ));
        assert!(matches!(
            service.query_phases(PhasesQuery {
                workspace_id: "ws-1".to_owned(),
            }),
            Err(MetricsQueryError::OwnerQueryNotEnrolled { .. })
        ));
    }
}
