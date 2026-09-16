//! Typed rows and test-only parity queries for the Go orchestrator's
//! `metrics.db` SQLite schema (`internal/metrics/db.go`).
//!
//! The raw SQL functions compile only in this crate's tests and receive an
//! in-memory or isolated fixture connection. Production live access is routed
//! through `crate::MetricsQueryService`; an independent pathname-based SQLite
//! reader is deliberately unavailable because it cannot retain authority
//! across WAL and writer-start races.
//!
//! Test-only parity surface (Go source, `internal/metrics/db.go`):
//! - `QueryMissions` → `query_missions`
//! - `QueryPersonaMetrics` → `query_persona_metrics`
//! - `QuerySkillUsage` → `query_skill_usage`
//! - `QueryTrends` → `query_trends`
//! - `QueryPhases` → `query_phases`
//! - `QueryRoutingMethodDistribution` → `query_routing_method_distribution`
//! - `FallbackRate` → [`fallback_rate`]
//! - `FallbackAlertThreshold` → [`FALLBACK_ALERT_THRESHOLD`]
//!
//! Not ported (write paths, deferred — see `PORTING.md` §0 LANE B):
//! `InitDB`'s schema/migrations, `RecordMission`, `UpsertMission`,
//! `UpsertMissionPhaseSnapshot`, `RecordSkillInvocation`,
//! `ImportFromJSONL`/`ImportMissingFromJSONL`, `InsertQuotaSnapshot`,
//! `InsertUsageSnapshot`. Consequently `openMetricsDB`'s JSONL backfill
//! (`internal/cmd/metrics.go:69-88`) is also not reproduced here: this
//! test-only parity adapter queries whatever is already in its fixture
//! `metrics.db` and does not import `metrics.jsonl` records first. A future
//! owner-mediated private view must perform that import without mutating the
//! live source.
//!
//! `QueryCostSummary`, `GetRecentSnapshots`, and `Get5hWindowTotals` are
//! also out of scope for this wave: the assignment's convergence surface is
//! `metrics personas/skills/trends/routing-methods/phases` plus the
//! top-level `metrics` mission list, per `PORT-ORDER.md` Wave 3.

#[cfg(test)]
use rusqlite::Connection;
use thiserror::Error;

/// Failures raised by the pure `metrics.db` queries.
#[derive(Debug, Error)]
pub enum MetricsReadError {
    /// A `SELECT` or row-scan failed. `what` names the Go function being
    /// mirrored, matching the `fmt.Errorf("querying ...: %w", err)` wraps.
    #[error("querying {what}: {source}")]
    Query {
        what: &'static str,
        #[source]
        source: rusqlite::Error,
    },
    /// A `started_at` column value did not parse as RFC3339, matching Go's
    /// `time.Parse(time.RFC3339, startedAt)` failure at `db.go:1268-1270`.
    #[error("parsing started_at {value:?}: not RFC3339")]
    InvalidStartedAt { value: String },
    /// A query that Go leaves unbounded returned more rows than
    /// [`crate::MAX_QUERY_LIMIT`].
    ///
    /// Refusing is deliberate. Silently truncating would hand a caller a page
    /// that looks complete, which is exactly the "arbitrary limit that changes
    /// correctness as data grows" failure the batch rules forbid — so an
    /// over-large result is an error the caller must see, not a shorter answer.
    #[error("{what} returned more than {limit} rows")]
    ResultTooLarge {
        /// Which Go function is being mirrored.
        what: &'static str,
        /// The bound that was exceeded.
        limit: usize,
    },
}

/// Mirrors Go's `MissionSummary` (`db.go:1115-1127`).
#[derive(Debug, Clone, PartialEq)]
pub struct MissionSummary {
    pub workspace_id: String,
    pub domain: String,
    pub status: String,
    pub decomp_source: String,
    pub task: String,
    pub top_persona: String,
    pub duration_sec: i64,
    pub phases_total: i64,
    pub phases_completed: i64,
    pub phases_failed: i64,
    /// RFC3339, validated but kept as the raw string — no `time.Time`
    /// equivalent is needed by any consumer of this adapter.
    pub started_at: String,
}

/// Mirrors Go's `PersonaMetric` (`db.go:1130-1138`).
#[derive(Debug, Clone, PartialEq)]
pub struct PersonaMetric {
    pub persona: String,
    pub phase_count: i64,
    pub avg_duration_sec: f64,
    /// 0-100.
    pub failure_rate: f64,
    pub avg_retries: f64,
    /// 0-100, percent selected by LLM.
    pub llm_pct: f64,
    /// 0-100, percent selected by keyword fallback.
    pub keyword_pct: f64,
}

/// Mirrors Go's `SkillUsage` (`db.go:1200-1207`).
#[derive(Debug, Clone, PartialEq)]
pub struct SkillUsage {
    pub skill_name: String,
    pub phase: String,
    pub persona: String,
    /// `"declared"` or `"output_parse"`.
    pub source: String,
    pub invocations: i64,
}

/// Mirrors Go's `DayTrend` (`db.go:1209-1215`).
#[derive(Debug, Clone, PartialEq)]
pub struct DayTrend {
    /// `"2026-03-05"`.
    pub day: String,
    pub total: i64,
    pub successes: i64,
    pub avg_duration: f64,
}

/// Mirrors Go's `PhaseRow` (`db.go:1369-1375`).
#[derive(Debug, Clone, PartialEq)]
pub struct PhaseRow {
    pub name: String,
    pub persona: String,
    pub status: String,
    pub duration_s: i64,
    pub parsed_skills: Vec<String>,
}

/// Mirrors Go's `RoutingMethodDist` (`db.go:1141-1145`).
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingMethodDist {
    pub method: String,
    pub count: i64,
    /// 0-100, rounded to one decimal by the SQL query itself.
    pub pct: f64,
}

/// Mirrors Go's `FallbackAlertThreshold` (`db.go:1149`).
pub const FALLBACK_ALERT_THRESHOLD: f64 = 30.0;

/// Mirrors Go's `FallbackRate` (`db.go:1153-1160`): returns the fallback
/// percentage from a distribution slice, or `0.0` if `"fallback"` is absent.
#[must_use]
pub fn fallback_rate(dist: &[RoutingMethodDist]) -> f64 {
    for d in dist {
        if d.method == "fallback" {
            return d.pct;
        }
    }
    0.0
}

/// Deserializes a stored JSON array string back to a `Vec<String>`. Mirrors
/// Go's `unmarshalSkillsJSON` (`db.go:600-611`): empty or invalid input
/// yields an empty vector (Go returns `nil`, which iterates identically to
/// an empty slice).
#[cfg(test)]
fn unmarshal_skills_json(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    serde_json::from_str(s).unwrap_or_default()
}

/// Strictly checks RFC3339 grammar and numeric ranges, mirroring Go's
/// `time.Parse(time.RFC3339, ...)` acceptance for the subset of inputs this
/// adapter observes (always-`Z`-suffixed, second-or-nanosecond precision
/// UTC timestamps written by `db.go`'s own `time.Now().UTC().Format(...)`
/// call sites). `orchestrator-core::codec::is_rfc3339` implements the same
/// grammar but is private to that crate; duplicated here rather than made
/// `pub` there, since this port's scope is `orchestrator-app` only —
/// flagged as a dedup opportunity for a later wave.
#[cfg(test)]
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
    matches!(
        bytes.get(19),
        Some(b'Z') | Some(b'.') | Some(b'+') | Some(b'-')
    )
}

/// Ports `DB.QueryMissions` (`db.go:1221-1274`): missions ordered newest
/// first, with optional domain/day/status/decomp-source/worker filters.
/// `limit <= 0` defaults to 20, matching Go. `worker == "ephemeral"` is a
/// CLI-facing alias for the empty string stored in `phases.worker_name`.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn query_missions(
    connection: &Connection,
    limit: i64,
    domain: &str,
    days: i64,
    status: &str,
    decomp_source: &str,
    worker: &str,
) -> Result<Vec<MissionSummary>, MetricsReadError> {
    // `limit <= 0` keeps Go's default of 20 for byte parity; anything above
    // `MAX_QUERY_LIMIT` is clamped so no caller-supplied number can turn this
    // into an unbounded scan.
    let limit =
        crate::metrics_owner::clamp_limit(limit, crate::metrics_owner::DEFAULT_MISSIONS_LIMIT);
    let worker_match = if worker == "ephemeral" { "" } else { worker };

    let mut statement = connection
        .prepare(
            r"
            SELECT m.id, m.domain, m.status, m.decomp_source, m.task, m.duration_s,
                   m.phases_total, m.phases_completed, m.phases_failed, m.started_at,
                   COALESCE((
                       SELECT p.persona FROM phases p
                       WHERE p.mission_id = m.id AND p.persona != ''
                       ORDER BY rowid LIMIT 1
                   ), '') AS top_persona
            FROM missions m
            WHERE (?1 = '' OR m.domain = ?1)
              AND (?2 = 0 OR m.started_at >= datetime('now', '-' || ?2 || ' days'))
              AND (?3 = '' OR m.status = ?3)
              AND (?4 = '' OR m.decomp_source = ?4)
              AND (?5 = '' OR EXISTS (
                  SELECT 1 FROM phases p2
                  WHERE p2.mission_id = m.id AND p2.worker_name = ?6
              ))
            ORDER BY m.started_at DESC
            LIMIT ?7
            ",
        )
        .map_err(|source| MetricsReadError::Query {
            what: "missions",
            source,
        })?;

    let rows = statement
        .query_map(
            rusqlite::params![
                domain,
                days,
                status,
                decomp_source,
                worker,
                worker_match,
                limit
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                ))
            },
        )
        .map_err(|source| MetricsReadError::Query {
            what: "missions",
            source,
        })?;

    let mut out = Vec::new();
    for row in rows {
        let (
            workspace_id,
            domain,
            status,
            decomp_source,
            task,
            duration_sec,
            phases_total,
            phases_completed,
            phases_failed,
            started_at,
            top_persona,
        ) = row.map_err(|source| MetricsReadError::Query {
            what: "missions",
            source,
        })?;
        if !is_rfc3339(&started_at) {
            return Err(MetricsReadError::InvalidStartedAt { value: started_at });
        }
        out.push(MissionSummary {
            workspace_id,
            domain,
            status,
            decomp_source,
            task,
            top_persona,
            duration_sec,
            phases_total,
            phases_completed,
            phases_failed,
            started_at,
        });
    }
    Ok(out)
}

/// Ports `DB.QueryPersonaMetrics` (`db.go:1277-1309`).
#[cfg(test)]
pub(crate) fn query_persona_metrics(
    connection: &Connection,
) -> Result<Vec<PersonaMetric>, MetricsReadError> {
    let mut statement = connection
        .prepare(
            r"
            SELECT
                persona,
                COUNT(*) AS phase_count,
                AVG(CAST(duration_s AS REAL)) AS avg_duration,
                SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) * 100.0 / COUNT(*) AS failure_rate,
                AVG(CAST(retries AS REAL)) AS avg_retries,
                SUM(CASE WHEN selection_method = 'llm' THEN 1 ELSE 0 END) * 100.0 / COUNT(*) AS llm_pct,
                SUM(CASE WHEN selection_method = 'keyword' THEN 1 ELSE 0 END) * 100.0 / COUNT(*) AS keyword_pct
            FROM phases
            WHERE persona != ''
            GROUP BY persona
            ORDER BY phase_count DESC
            ",
        )
        .map_err(|source| MetricsReadError::Query {
            what: "persona metrics",
            source,
        })?;

    let rows = statement
        .query_map([], |row| {
            Ok(PersonaMetric {
                persona: row.get(0)?,
                phase_count: row.get(1)?,
                avg_duration_sec: row.get(2)?,
                failure_rate: row.get(3)?,
                avg_retries: row.get(4)?,
                llm_pct: row.get(5)?,
                keyword_pct: row.get(6)?,
            })
        })
        .map_err(|source| MetricsReadError::Query {
            what: "persona metrics",
            source,
        })?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|source| MetricsReadError::Query {
            what: "persona metrics",
            source,
        })
}

/// Ports `DB.QuerySkillUsage` (`db.go:1312-1334`).
#[cfg(test)]
pub(crate) fn query_skill_usage(
    connection: &Connection,
) -> Result<Vec<SkillUsage>, MetricsReadError> {
    let mut statement = connection
        .prepare(
            r"
            SELECT skill_name, phase, persona, source, COUNT(*) AS invocations
            FROM skill_invocations
            GROUP BY skill_name, phase, persona, source
            ORDER BY invocations DESC, skill_name, phase
            LIMIT 100
            ",
        )
        .map_err(|source| MetricsReadError::Query {
            what: "skill usage",
            source,
        })?;

    let rows = statement
        .query_map([], |row| {
            Ok(SkillUsage {
                skill_name: row.get(0)?,
                phase: row.get(1)?,
                persona: row.get(2)?,
                source: row.get(3)?,
                invocations: row.get(4)?,
            })
        })
        .map_err(|source| MetricsReadError::Query {
            what: "skill usage",
            source,
        })?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|source| MetricsReadError::Query {
            what: "skill usage",
            source,
        })
}

/// Ports `DB.QueryTrends` (`db.go:1337-1366`). `days <= 0` defaults to 30,
/// matching Go.
#[cfg(test)]
pub(crate) fn query_trends(
    connection: &Connection,
    days: i64,
) -> Result<Vec<DayTrend>, MetricsReadError> {
    let days = if days <= 0 { 30 } else { days };
    let mut statement = connection
        .prepare(
            r"
            SELECT
                date(started_at) AS day,
                COUNT(*) AS total,
                SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) AS successes,
                AVG(CAST(duration_s AS REAL)) AS avg_duration
            FROM missions
            WHERE started_at >= datetime('now', '-' || ?1 || ' days')
            GROUP BY day
            ORDER BY day DESC
            ",
        )
        .map_err(|source| MetricsReadError::Query {
            what: "trends",
            source,
        })?;

    let rows = statement
        .query_map(rusqlite::params![days], |row| {
            Ok(DayTrend {
                day: row.get(0)?,
                total: row.get(1)?,
                successes: row.get(2)?,
                avg_duration: row.get(3)?,
            })
        })
        .map_err(|source| MetricsReadError::Query {
            what: "trends",
            source,
        })?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|source| MetricsReadError::Query {
            what: "trends",
            source,
        })
}

/// Ports `DB.QueryPhases` (`db.go:1378-1401`).
#[cfg(test)]
pub(crate) fn query_phases(
    connection: &Connection,
    mission_id: &str,
) -> Result<Vec<PhaseRow>, MetricsReadError> {
    let mut statement = connection
        .prepare(
            r"
            SELECT name, persona, status, duration_s, COALESCE(parsed_skills, '')
            FROM phases
            WHERE mission_id = ?1
            ORDER BY rowid
            LIMIT ?2
            ",
        )
        .map_err(|source| MetricsReadError::Query {
            what: "phases",
            source,
        })?;

    // One row past the bound, so a mission that exceeds it is *detected*
    // rather than quietly shortened. Go has no limit here; this preserves its
    // output byte-for-byte up to the bound and refuses beyond it.
    let probe = i64::try_from(crate::metrics_owner::MAX_QUERY_LIMIT)
        .unwrap_or(i64::MAX)
        .saturating_add(1);
    let rows = statement
        .query_map(rusqlite::params![mission_id, probe], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|source| MetricsReadError::Query {
            what: "phases",
            source,
        })?;

    let mut out = Vec::new();
    for row in rows {
        let (name, persona, status, duration_s, parsed_skills_json) =
            row.map_err(|source| MetricsReadError::Query {
                what: "phases",
                source,
            })?;
        out.push(PhaseRow {
            name,
            persona,
            status,
            duration_s,
            parsed_skills: unmarshal_skills_json(&parsed_skills_json),
        });
    }
    if out.len() > crate::metrics_owner::MAX_QUERY_LIMIT {
        return Err(MetricsReadError::ResultTooLarge {
            what: "phases",
            limit: crate::metrics_owner::MAX_QUERY_LIMIT,
        });
    }
    Ok(out)
}

/// Ports `DB.QueryRoutingMethodDistribution` (`db.go:1166-1198`).
/// `"required_review"` phases are excluded (auto-injected gates, not organic
/// routing decisions). Rows are ordered by count descending.
#[cfg(test)]
pub(crate) fn query_routing_method_distribution(
    connection: &Connection,
) -> Result<Vec<RoutingMethodDist>, MetricsReadError> {
    let mut statement = connection
        .prepare(
            r"
            SELECT
                COALESCE(NULLIF(selection_method, ''), 'unknown') AS method,
                COUNT(*) AS cnt,
                COALESCE(
                    COUNT(*) * 100.0 / NULLIF((
                        SELECT COUNT(*) FROM phases
                        WHERE persona != ''
                          AND COALESCE(selection_method, '') != 'required_review'
                    ), 0),
                0.0) AS pct
            FROM phases
            WHERE persona != ''
              AND COALESCE(selection_method, '') != 'required_review'
            GROUP BY method
            ORDER BY cnt DESC
            ",
        )
        .map_err(|source| MetricsReadError::Query {
            what: "routing method distribution",
            source,
        })?;

    let rows = statement
        .query_map([], |row| {
            Ok(RoutingMethodDist {
                method: row.get(0)?,
                count: row.get(1)?,
                pct: row.get(2)?,
            })
        })
        .map_err(|source| MetricsReadError::Query {
            what: "routing method distribution",
            source,
        })?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|source| MetricsReadError::Query {
            what: "routing method distribution",
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    /// Test-only schema replica of the subset of `db.go`'s DDL these read
    /// queries touch. Production schema creation/migration is the LANE B
    /// write path and is not ported here (see module doc comment) — this
    /// mirrors just enough of `initSchema` (`db.go:265-443`) to seed
    /// fixtures for the read functions under test.
    fn seed_schema(connection: &Connection) -> TestResult {
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
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_mission(
        connection: &Connection,
        id: &str,
        domain: &str,
        status: &str,
        started_at: &str,
        duration_s: i64,
        phases_total: i64,
        phases_completed: i64,
        phases_failed: i64,
    ) -> TestResult {
        connection.execute(
            "INSERT INTO missions (id, domain, task, started_at, status, duration_s, phases_total, phases_completed, phases_failed) VALUES (?1, ?2, '', ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![id, domain, started_at, status, duration_s, phases_total, phases_completed, phases_failed],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_phase(
        connection: &Connection,
        id: &str,
        mission_id: &str,
        name: &str,
        persona: &str,
        selection_method: &str,
        duration_s: i64,
        status: &str,
        retries: i64,
    ) -> TestResult {
        connection.execute(
            "INSERT INTO phases (id, mission_id, name, persona, selection_method, duration_s, status, retries) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![id, mission_id, name, persona, selection_method, duration_s, status, retries],
        )?;
        Ok(())
    }

    fn insert_skill(
        connection: &Connection,
        mission_id: &str,
        phase: &str,
        persona: &str,
        skill_name: &str,
        source: &str,
    ) -> TestResult {
        connection.execute(
            "INSERT INTO skill_invocations (mission_id, phase, persona, skill_name, source) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![mission_id, phase, persona, skill_name, source],
        )?;
        Ok(())
    }

    fn open_memory() -> TestResult<Connection> {
        let connection = Connection::open_in_memory()?;
        seed_schema(&connection)?;
        Ok(connection)
    }

    /// Ports `TestPersonaStats`'s "QueryPersonaMetrics aggregate counts"
    /// subtest (`db_test.go:351-398`).
    #[test]
    fn query_persona_metrics_aggregate_counts() -> TestResult {
        let connection = open_memory()?;
        insert_mission(
            &connection,
            "ws-stats-1",
            "dev",
            "success",
            "2026-01-01T00:00:00Z",
            3600,
            2,
            2,
            0,
        )?;
        insert_mission(
            &connection,
            "ws-stats-2",
            "dev",
            "failure",
            "2026-01-01T01:00:00Z",
            600,
            1,
            0,
            1,
        )?;
        insert_mission(
            &connection,
            "ws-stats-3",
            "creative",
            "success",
            "2026-01-01T02:00:00Z",
            300,
            2,
            2,
            0,
        )?;

        insert_phase(
            &connection,
            "ws-stats-1_phase-1",
            "ws-stats-1",
            "phase-1",
            "backend-engineer",
            "llm",
            60,
            "completed",
            0,
        )?;
        insert_phase(
            &connection,
            "ws-stats-1_phase-2",
            "ws-stats-1",
            "phase-2",
            "backend-engineer",
            "keyword",
            90,
            "completed",
            0,
        )?;
        insert_phase(
            &connection,
            "ws-stats-2_phase-1",
            "ws-stats-2",
            "phase-1",
            "backend-engineer",
            "llm",
            120,
            "failed",
            2,
        )?;
        insert_phase(
            &connection,
            "ws-stats-3_phase-1",
            "ws-stats-3",
            "phase-1",
            "architect",
            "llm",
            45,
            "completed",
            0,
        )?;
        insert_phase(
            &connection,
            "ws-stats-3_phase-2",
            "ws-stats-3",
            "phase-2",
            "architect",
            "llm",
            55,
            "completed",
            0,
        )?;

        let metrics = query_persona_metrics(&connection)?;
        assert_eq!(metrics.len(), 2);

        let backend = &metrics[0];
        assert_eq!(backend.persona, "backend-engineer");
        assert_eq!(backend.phase_count, 3);
        let want_fail_rate = 1.0 / 3.0 * 100.0;
        assert!((backend.failure_rate - want_fail_rate).abs() < 0.1);
        let want_llm = 2.0 / 3.0 * 100.0;
        assert!((backend.llm_pct - want_llm).abs() < 0.1);
        let want_retries = 2.0 / 3.0;
        assert!((backend.avg_retries - want_retries).abs() < 0.01);

        let architect = &metrics[1];
        assert_eq!(architect.persona, "architect");
        assert_eq!(architect.phase_count, 2);
        assert_eq!(architect.failure_rate, 0.0);
        assert_eq!(architect.llm_pct, 100.0);
        Ok(())
    }

    /// Ports `TestPersonaStats`'s "QuerySkillUsage counts and ordering"
    /// subtest (`db_test.go:400-419`).
    #[test]
    fn query_skill_usage_counts_and_ordering() -> TestResult {
        let connection = open_memory()?;
        insert_mission(
            &connection,
            "ws-stats-1",
            "dev",
            "success",
            "2026-01-01T00:00:00Z",
            3600,
            2,
            2,
            0,
        )?;
        insert_mission(
            &connection,
            "ws-stats-2",
            "dev",
            "failure",
            "2026-01-01T01:00:00Z",
            600,
            1,
            0,
            1,
        )?;
        insert_mission(
            &connection,
            "ws-stats-3",
            "creative",
            "success",
            "2026-01-01T02:00:00Z",
            300,
            2,
            2,
            0,
        )?;
        insert_skill(
            &connection,
            "ws-stats-1",
            "phase-1",
            "backend-engineer",
            "obsidian",
            "declared",
        )?;
        insert_skill(
            &connection,
            "ws-stats-1",
            "phase-1",
            "backend-engineer",
            "obsidian",
            "declared",
        )?;
        insert_skill(
            &connection,
            "ws-stats-2",
            "phase-1",
            "backend-engineer",
            "scout",
            "declared",
        )?;
        insert_skill(
            &connection,
            "ws-stats-3",
            "phase-1",
            "architect",
            "obsidian",
            "declared",
        )?;

        let usage = query_skill_usage(&connection)?;
        assert_eq!(usage.len(), 3);
        let top = &usage[0];
        assert_eq!(top.skill_name, "obsidian");
        assert_eq!(top.phase, "phase-1");
        assert_eq!(top.persona, "backend-engineer");
        assert_eq!(top.invocations, 2);
        Ok(())
    }

    /// Ports `TestPersonaStats`'s `QueryMissions` subtests
    /// (`db_test.go:446-485`).
    #[test]
    fn query_missions_filters() -> TestResult {
        let connection = open_memory()?;
        insert_mission(
            &connection,
            "ws-stats-1",
            "dev",
            "success",
            "2026-01-01T00:00:00Z",
            3600,
            2,
            2,
            0,
        )?;
        insert_mission(
            &connection,
            "ws-stats-2",
            "dev",
            "failure",
            "2026-01-01T01:00:00Z",
            600,
            1,
            0,
            1,
        )?;
        insert_mission(
            &connection,
            "ws-stats-3",
            "creative",
            "success",
            "2026-01-01T02:00:00Z",
            300,
            2,
            2,
            0,
        )?;
        insert_phase(
            &connection,
            "ws-stats-1_phase-1",
            "ws-stats-1",
            "phase-1",
            "backend-engineer",
            "llm",
            60,
            "completed",
            0,
        )?;
        insert_phase(
            &connection,
            "ws-stats-3_phase-1",
            "ws-stats-3",
            "phase-1",
            "architect",
            "llm",
            45,
            "completed",
            0,
        )?;

        let dev_rows = query_missions(&connection, 20, "dev", 0, "", "", "")?;
        assert_eq!(dev_rows.len(), 2);
        assert!(dev_rows.iter().all(|r| r.domain == "dev"));

        let failure_rows = query_missions(&connection, 20, "", 0, "failure", "", "")?;
        assert_eq!(failure_rows.len(), 1);
        assert_eq!(failure_rows[0].workspace_id, "ws-stats-2");

        let success_rows = query_missions(&connection, 20, "", 0, "success", "", "")?;
        assert!(success_rows.iter().all(|r| !r.top_persona.is_empty()));
        Ok(())
    }

    /// Ports `TestQueryRoutingMethodDistribution` (`db_test.go:1485-1600`).
    #[test]
    fn routing_method_distribution_empty_database() -> TestResult {
        let connection = open_memory()?;
        let dist = query_routing_method_distribution(&connection)?;
        assert!(dist.is_empty());
        Ok(())
    }

    #[test]
    fn routing_method_distribution_counts_and_percentages() -> TestResult {
        let connection = open_memory()?;
        insert_mission(
            &connection,
            "ws-rmd-1",
            "dev",
            "success",
            "2026-01-01T00:00:00Z",
            3600,
            8,
            8,
            0,
        )?;
        let phases = [
            ("p1", "architect", "llm"),
            ("p2", "architect", "llm"),
            ("p3", "backend-engineer", "llm"),
            ("p4", "backend-engineer", "llm"),
            ("p5", "backend-engineer", "keyword"),
            ("p6", "backend-engineer", "keyword"),
            ("p7", "backend-engineer", "fallback"),
            ("p8", "staff-code-reviewer", "required_review"),
        ];
        for (name, persona, method) in phases {
            insert_phase(
                &connection,
                &format!("ws-rmd-1_{name}"),
                "ws-rmd-1",
                name,
                persona,
                method,
                10,
                "completed",
                0,
            )?;
        }

        let dist = query_routing_method_distribution(&connection)?;
        assert_eq!(dist.len(), 3);
        assert_eq!(dist[0].method, "llm");
        assert_eq!(dist[0].count, 4);

        let by_method = |method: &str| dist.iter().find(|d| d.method == method);
        let llm = by_method("llm").ok_or("expected an llm row")?;
        let want_llm_pct = 4.0 / 7.0 * 100.0;
        assert!((llm.pct - want_llm_pct).abs() < 0.1);
        let keyword = by_method("keyword").ok_or("expected a keyword row")?;
        let want_kw_pct = 2.0 / 7.0 * 100.0;
        assert!((keyword.pct - want_kw_pct).abs() < 0.1);
        let fallback = by_method("fallback").ok_or("expected a fallback row")?;
        let want_fb_pct = 1.0 / 7.0 * 100.0;
        assert!((fallback.pct - want_fb_pct).abs() < 0.1);
        assert!(by_method("required_review").is_none());
        Ok(())
    }

    #[test]
    fn routing_method_distribution_required_review_only_returns_empty() -> TestResult {
        let connection = open_memory()?;
        insert_mission(
            &connection,
            "ws-rmd-2",
            "dev",
            "success",
            "2026-01-01T00:00:00Z",
            1800,
            1,
            1,
            0,
        )?;
        insert_phase(
            &connection,
            "ws-rmd-2_review",
            "ws-rmd-2",
            "review",
            "staff-code-reviewer",
            "required_review",
            60,
            "completed",
            0,
        )?;

        let dist = query_routing_method_distribution(&connection)?;
        assert!(dist.is_empty());
        Ok(())
    }

    /// Ports `TestFallbackRate` (`db_test.go:1602-1654`).
    #[test]
    fn fallback_rate_cases() {
        assert_eq!(fallback_rate(&[]), 0.0);
        assert_eq!(
            fallback_rate(&[
                RoutingMethodDist {
                    method: "llm".into(),
                    count: 8,
                    pct: 80.0
                },
                RoutingMethodDist {
                    method: "keyword".into(),
                    count: 2,
                    pct: 20.0
                },
            ]),
            0.0
        );
        assert_eq!(
            fallback_rate(&[
                RoutingMethodDist {
                    method: "llm".into(),
                    count: 5,
                    pct: 50.0
                },
                RoutingMethodDist {
                    method: "keyword".into(),
                    count: 2,
                    pct: 20.0
                },
                RoutingMethodDist {
                    method: "fallback".into(),
                    count: 3,
                    pct: 30.0
                },
            ]),
            30.0
        );
        assert_eq!(FALLBACK_ALERT_THRESHOLD, 30.0);
    }

    /// Ports `QueryTrends returns today's data` (`db_test.go:421-444`),
    /// adapted to a fixed seeded date rather than `time.Now()` so the test
    /// is deterministic without a clock dependency.
    #[test]
    fn query_trends_totals() -> TestResult {
        let connection = open_memory()?;
        // All three missions share "today" relative to SQLite's own
        // `datetime('now', ...)`, matching Go's use of the same DB-side
        // clock rather than an injected one.
        let now = connection.query_row("SELECT datetime('now')", [], |r| r.get::<_, String>(0))?;
        let started = format!("{}Z", now.replace(' ', "T"));
        insert_mission(
            &connection,
            "ws-t1",
            "dev",
            "success",
            &started,
            60,
            1,
            1,
            0,
        )?;
        insert_mission(
            &connection,
            "ws-t2",
            "dev",
            "failure",
            &started,
            60,
            1,
            0,
            1,
        )?;
        insert_mission(
            &connection,
            "ws-t3",
            "creative",
            "success",
            &started,
            60,
            1,
            1,
            0,
        )?;

        let trends = query_trends(&connection, 7)?;
        let total: i64 = trends.iter().map(|t| t.total).sum();
        assert_eq!(total, 3);
        let successes: i64 = trends.iter().map(|t| t.successes).sum();
        assert_eq!(successes, 2);
        Ok(())
    }

    /// Ports `TestPhaseNameCollision`-adjacent read behavior: `QueryPhases`
    /// returns rows ordered by insertion (`rowid`) and decodes
    /// `parsed_skills`.
    #[test]
    fn query_phases_returns_ordered_rows_with_parsed_skills() -> TestResult {
        let connection = open_memory()?;
        insert_mission(
            &connection,
            "ws-p1",
            "dev",
            "success",
            "2026-01-01T00:00:00Z",
            60,
            2,
            2,
            0,
        )?;
        connection.execute(
            "INSERT INTO phases (id, mission_id, name, persona, status, duration_s, parsed_skills) VALUES ('ws-p1_a', 'ws-p1', 'a', 'architect', 'completed', 10, '[\"obsidian\",\"scout\"]')",
            [],
        )?;
        connection.execute(
            "INSERT INTO phases (id, mission_id, name, persona, status, duration_s, parsed_skills) VALUES ('ws-p1_b', 'ws-p1', 'b', 'architect', 'completed', 20, '')",
            [],
        )?;

        let rows = query_phases(&connection, "ws-p1")?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "a");
        assert_eq!(rows[0].parsed_skills, vec!["obsidian", "scout"]);
        assert_eq!(rows[1].name, "b");
        assert!(rows[1].parsed_skills.is_empty());
        Ok(())
    }

    #[test]
    fn unmarshal_skills_json_tolerates_malformed_input() {
        assert!(unmarshal_skills_json("").is_empty());
        assert!(unmarshal_skills_json("not json").is_empty());
        assert_eq!(unmarshal_skills_json(r#"["a","b"]"#), vec!["a", "b"]);
    }
}
