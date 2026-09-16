//! The one metrics owner (B3-DESIGN §4) and the evidence that gates it (§Gate 6).
//!
//! # Why one owner
//!
//! `metrics.db` is Go-shaped and Go-readable. Two writers would race on the
//! same rows with no shared lock, so [`MetricsOwner::assume`] takes a
//! [`MetricsOwnerCapability`] *by value* and holds it for the owner's whole
//! life. That capability is a kernel `flock` on the home, so a second
//! `assume` against the same home fails rather than quietly interleaving.
//!
//! # Why phases survive a crash
//!
//! TRK-493 records a 76% phase-row loss in the Go engine, caused by two
//! independent conditions on the write path:
//!
//! - the per-phase write `recordPhaseSkillsDB` (`internal/engine/metrics.go:311`)
//!   returns early at `metrics.go:315-317` when the phase declared and parsed no
//!   skills, so a skill-less phase never wrote a row of its own; and
//! - the end-of-mission batch `RecordMetrics` (`internal/engine/metrics.go:144`)
//!   never runs when the process dies.
//!
//! A phase therefore landed in `metrics.db` only if it invoked a skill *or* its
//! mission finished cleanly. Both conditions are gone here:
//!
//! 1. [`PhaseMetricIntent::parsed_skills`] may be empty. There is no
//!    `is_empty()` branch anywhere on the enqueue path — emptiness is data.
//! 2. The intent is committed to `runtime.db`'s publication queue *inside the
//!    journal transaction that records the phase transition*, so it is durable
//!    the instant the transition is.
//! 3. [`MetricsOwner::record_phase`] upserts on the Go phase-row key, so
//!    redelivery after a crash is a no-op.
//!
//! # Why an ambiguous verification can never be a pass
//!
//! [`PhaseMetricIntent::new`] cannot be constructed without an
//! [`ExactProcessGroupAbsence`] witness, so a phase whose background children
//! are still alive has no representable metric at all — the gate stays blocked
//! rather than recording an optimistic row. And `gate_passed` is derived from
//! [`orchestrator_core::VerificationOutcome::gate_passed`], which is true only
//! for `Classified(Pass)`; `Skip`, `NoTests`, `Timeout`, and
//! `InfrastructureError` all render as a non-passing gate.

use std::path::Path;

use orchestrator_core::{
    MissionId, VerificationClass, VerificationOutcome, VerificationSummary,
    VerificationTermination, classify_verification,
};
use orchestrator_knowledge::{Redacted, Redactor};
use orchestrator_process::ExactProcessGroupAbsence;
use rusqlite::{Connection, OpenFlags, params};
use thiserror::Error;

use crate::{
    capability::{CapabilityError, MetricsOwnerCapability, OwnerLeaseError},
    metrics_query::{
        MetricsQueryError, MetricsQueryService, PrivateRunMetricsQuery, PrivateRunMetricsResult,
    },
    runtime_store::{ClaimedPublication, StorageActorAuthority},
    worker_spawn::SkillRef,
};

/// Filename of the Go metrics store inside a runtime home
/// (`internal/metrics/db.go:219`).
const METRICS_DB_FILE: &str = "metrics.db";

/// Upper bound on any single owner query or drain page.
///
/// Every request is clamped to this, so no code path can ask the database for
/// an unbounded row count regardless of caller input.
pub const MAX_QUERY_LIMIT: usize = 1_000;

/// Go's default mission-list size for a non-positive limit
/// (`internal/cmd/metrics.go`). Preserved for byte parity.
pub const DEFAULT_MISSIONS_LIMIT: i64 = 20;

/// Longest error message retained on a phase row, after redaction.
const MAX_ERROR_MESSAGE_BYTES: usize = 2_048;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Failures raised by the metrics owner.
///
/// No variant carries provider output, record bodies, or SQL. Where a message
/// is genuinely needed it is a [`Redacted<String>`], whose `Display` and `Debug`
/// both print a placeholder — so a caller that logs this error with `{}` or
/// `{:?}` cannot leak a credential that appeared in a worker's stderr.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MetricsOwnerError {
    /// Another metrics owner already holds this home's lease.
    #[error(transparent)]
    Lease(#[from] OwnerLeaseError),
    /// The home's identity changed under the owner.
    #[error(transparent)]
    Capability(#[from] CapabilityError),
    /// A statement against `metrics.db` failed. The SQL is never carried.
    #[error("metrics store operation failed during {operation}")]
    Store {
        /// Stable, non-content operation name.
        operation: &'static str,
    },
    /// A queued publication was not a metrics intent this owner understands.
    #[error("publication type {type_name} is not a metrics intent")]
    UnknownIntent {
        /// The registered type name that was refused.
        type_name: String,
    },
    /// A queued metrics intent could not be decoded.
    ///
    /// The context is redacted because a malformed payload may be malformed
    /// *because* it contains something that should never have been stored.
    #[error("metrics intent payload is malformed: {context}")]
    MalformedIntent {
        /// Redacted decoding context.
        context: Redacted<String>,
    },
    /// A phase metric was offered without a proven-absent process group.
    #[error("phase {phase} has unreaped background processes; its metric is not recordable")]
    ProcessGroupNotReaped {
        /// The phase whose group is still alive.
        phase: String,
    },
    /// The typed owner query refused malformed or over-large stored metrics.
    #[error(transparent)]
    Query(#[from] MetricsQueryError),
}

impl MetricsOwnerError {
    const fn store(operation: &'static str) -> Self {
        Self::Store { operation }
    }
}

// ---------------------------------------------------------------------------
// Intents
// ---------------------------------------------------------------------------

/// Token counts carried on a phase and aggregated onto its mission.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TokenCounts {
    /// Input tokens.
    pub input: i64,
    /// Output tokens.
    pub output: i64,
    /// Cache-creation tokens.
    pub cache_creation: i64,
    /// Cache-read tokens.
    pub cache_read: i64,
}

/// Terminal status of one phase, mirroring Go's `phases.status` values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhaseStatus {
    /// The phase finished and its gate passed.
    Completed,
    /// The phase finished and its gate did not pass.
    Failed,
    /// The phase was not run.
    Skipped,
}

impl PhaseStatus {
    /// The Go string written to `phases.status`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

/// One phase's terminal metrics, ready to be enqueued or written.
///
/// Construct with [`PhaseMetricIntent::new`], which requires the reaping
/// witness. The struct's fields are public for ergonomics *after* that gate,
/// but `reaping` is private so the witness cannot be dropped from an intent
/// that already carries one.
#[derive(Debug)]
pub struct PhaseMetricIntent {
    /// Owning mission.
    pub mission: MissionId,
    /// Phase identifier, used verbatim in the Go row key.
    pub phase: String,
    /// Which logical attempt produced these numbers.
    pub logical_attempt: u32,
    /// Persona that ran the phase.
    pub persona: String,
    /// How the persona was selected (`llm`, `keyword`, `fallback`, …).
    pub selection_method: String,
    /// Terminal status.
    pub status: PhaseStatus,
    /// Wall-clock seconds.
    pub duration_s: i64,
    /// Retries consumed.
    pub retries: i64,
    /// Whether the verification gate passed. Derived, never asserted: see
    /// [`PhaseMetricIntent::from_verification`].
    pub gate_passed: bool,
    /// Provider that served the phase.
    pub provider: String,
    /// Model that served the phase.
    pub model: String,
    /// Observed execution effort, when the runtime exposes it.
    pub effort: String,
    /// Token counts.
    pub tokens: TokenCounts,
    /// Whether every token field was present in the provider's terminal wire.
    pub tokens_known: bool,
    /// Cost in USD.
    pub cost_usd: f64,
    /// Whether `cost_usd` was reported by the provider rather than imputed.
    pub cost_known: bool,
    /// Whether the durable release protocol confirmed the target, rather than
    /// only a cleanup launcher, crossed the release boundary.
    pub target_released: bool,
    /// Persistent worker display name.
    pub worker_name: String,
    /// Skills parsed from the phase's output.
    ///
    /// **May be empty.** No code path treats emptiness as a reason to skip the
    /// write — that early return is the TRK-493 defect this port removes.
    pub parsed_skills: Vec<SkillRef>,
    /// Error class, when the phase failed.
    pub error_type: String,
    /// Error text, when the phase failed. Redacted on the way to storage.
    pub error_message: String,
    /// Proof that the phase's process group is gone.
    reaping: ExactProcessGroupAbsence,
}

impl PhaseMetricIntent {
    /// Builds a phase metric around a proven-absent process group.
    ///
    /// The witness is the whole point of the signature: without one this type
    /// cannot exist, so a phase that left a background child alive has no
    /// representable metric and cannot be recorded as anything — least of all
    /// as a pass.
    #[must_use]
    pub fn new(
        mission: MissionId,
        phase: impl Into<String>,
        logical_attempt: u32,
        reaping: ExactProcessGroupAbsence,
    ) -> Self {
        Self {
            mission,
            phase: phase.into(),
            logical_attempt,
            persona: String::new(),
            selection_method: String::new(),
            status: PhaseStatus::Failed,
            duration_s: 0,
            retries: 0,
            gate_passed: false,
            provider: String::new(),
            model: String::new(),
            effort: String::new(),
            tokens: TokenCounts::default(),
            tokens_known: true,
            cost_usd: 0.0,
            cost_known: true,
            target_released: false,
            worker_name: String::new(),
            parsed_skills: Vec::new(),
            error_type: String::new(),
            error_message: String::new(),
            reaping,
        }
    }

    /// Applies a verification outcome to `status` and `gate_passed` together.
    ///
    /// Going through this method rather than setting the two fields by hand is
    /// what keeps them consistent: `Skip`, `NoTests`, `Timeout`, and
    /// `InfrastructureError` all yield `gate_passed == false`, because
    /// [`VerificationOutcome::gate_passed`] is true only for
    /// `Classified(Pass)`.
    #[must_use]
    pub fn from_verification(mut self, outcome: VerificationOutcome) -> Self {
        self.gate_passed = outcome.gate_passed();
        self.status = if outcome.gate_passed() {
            PhaseStatus::Completed
        } else {
            PhaseStatus::Failed
        };
        self.error_type = match outcome {
            VerificationOutcome::Classified(VerificationClass::Pass) => String::new(),
            VerificationOutcome::Classified(class) => verification_class_name(class).to_owned(),
            VerificationOutcome::Cancelled => "cancelled".to_owned(),
        };
        self
    }

    /// The proof that this phase's process group was reaped.
    #[must_use]
    pub const fn reaping(&self) -> &ExactProcessGroupAbsence {
        &self.reaping
    }

    /// The Go phase-row primary key: `mission_id + "_" + phase`
    /// (`internal/metrics/db.go:618`).
    ///
    /// Deliberately excludes the logical attempt. Go collapses retries onto one
    /// row and records the count in `phases.retries`; keying by attempt would
    /// produce N rows per phase and break every Go reader's `phases_total`
    /// arithmetic. Because the key is stable across attempts, a redelivered
    /// publication upserts identical values (a no-op) and a genuinely later
    /// attempt supersedes the row — which is exactly `upsertPhase`'s semantics.
    #[must_use]
    pub fn row_key(&self) -> String {
        format!("{}_{}", self.mission.as_str(), self.phase)
    }
}

/// Mission-level aggregates written when a mission reaches a terminal state.
///
/// This is enrichment over phase rows that already exist, never the thing that
/// creates them — that inversion is what made a crashed Go mission lose every
/// phase it had run.
#[derive(Clone, Debug, Default)]
pub struct TerminalMetricIntent {
    /// Mission workspace identifier.
    pub mission: String,
    /// Mission domain.
    pub domain: String,
    /// Mission task text.
    pub task: String,
    /// RFC3339 start.
    pub started_at: String,
    /// RFC3339 finish.
    pub finished_at: String,
    /// Wall-clock seconds.
    pub duration_s: i64,
    /// Terminal mission status (`completed`, `failed`, or `cancelled` for the
    /// durable authored pilot; compatibility callers may retain legacy text).
    pub status: String,
    /// How the mission was decomposed.
    pub decomp_source: String,
}

// ---------------------------------------------------------------------------
// The owner
// ---------------------------------------------------------------------------

/// The single writer of one runtime home's `metrics.db`.
///
/// No other type in the workspace opens `metrics.db` for writing:
/// `metrics_read.rs`'s SQL helpers are `#[cfg(test)]`,
/// [`crate::MetricsQueryService`] is read-only and typed, and
/// [`crate::UnenrolledMetricsQueryService`] has no write surface at all.
pub struct MetricsOwner {
    connection: Connection,
    capability: MetricsOwnerCapability,
    actor: StorageActorAuthority,
    redactor: Redactor,
}

impl MetricsOwner {
    /// Assumes exclusive ownership of a home's metrics store.
    ///
    /// Takes the capability by value: the kernel lease lives as long as the
    /// owner, so a second `assume` against the same home fails with
    /// [`CapabilityError::LeaseHeld`] while the first owner is alive.
    ///
    /// # Errors
    /// Returns [`MetricsOwnerError::Capability`] when the home's identity
    /// changed, and [`MetricsOwnerError::Store`] when the store cannot be
    /// opened or its Go schema cannot be applied.
    pub fn assume(capability: MetricsOwnerCapability) -> Result<Self, MetricsOwnerError> {
        capability.verify()?;
        let path = capability.home_path().join(METRICS_DB_FILE);
        let connection = open_metrics_db(&path)?;
        apply_go_schema(&connection)?;
        Ok(Self {
            connection,
            capability,
            actor: StorageActorAuthority::new(),
            redactor: Redactor::new(),
        })
    }

    /// Writes one phase's terminal metrics.
    ///
    /// Idempotent: the upsert is keyed on [`PhaseMetricIntent::row_key`], so
    /// redelivering a publication that was committed before a crash rewrites
    /// identical values.
    ///
    /// # Errors
    /// Returns [`MetricsOwnerError::Store`] when the upsert fails.
    pub fn record_phase(&self, intent: &PhaseMetricIntent) -> Result<(), MetricsOwnerError> {
        self.capability.verify()?;
        // A phase row references a mission row. Go creates the mission first;
        // a crashed mission may never have got that far, so ensure a stub
        // exists rather than dropping the phase — losing the phase is the
        // failure mode this whole module exists to remove.
        self.ensure_mission_stub(intent.mission.as_str())?;
        let skills: Vec<&str> = intent
            .parsed_skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect();
        self.connection
            .execute(
                "INSERT INTO phases (
                    id, mission_id, name, persona, selection_method, duration_s,
                    status, retries, gate_passed, error_type, error_message,
                    provider, model, effort, tokens_in, tokens_out, tokens_cache_creation,
                    tokens_cache_read, tokens_known, cost_usd, cost_known, target_released,
                    parsed_skills, worker_name
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                           ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)
                 ON CONFLICT(id) DO UPDATE SET
                    mission_id = excluded.mission_id,
                    name = excluded.name,
                    persona = excluded.persona,
                    selection_method = excluded.selection_method,
                    duration_s = excluded.duration_s,
                    status = excluded.status,
                    retries = excluded.retries,
                    gate_passed = excluded.gate_passed,
                    error_type = excluded.error_type,
                    error_message = excluded.error_message,
                    provider = excluded.provider,
                    model = excluded.model,
                    effort = excluded.effort,
                    tokens_in = excluded.tokens_in,
                    tokens_out = excluded.tokens_out,
                    tokens_cache_creation = excluded.tokens_cache_creation,
                    tokens_cache_read = excluded.tokens_cache_read,
                    tokens_known = excluded.tokens_known,
                    cost_usd = excluded.cost_usd,
                    cost_known = excluded.cost_known,
                    target_released = excluded.target_released,
                    parsed_skills = excluded.parsed_skills,
                    worker_name = excluded.worker_name",
                params![
                    intent.row_key(),
                    intent.mission.as_str(),
                    intent.phase,
                    intent.persona,
                    intent.selection_method,
                    intent.duration_s,
                    intent.status.as_str(),
                    intent.retries,
                    i64::from(intent.gate_passed),
                    intent.error_type,
                    self.redact(&intent.error_message),
                    intent.provider,
                    intent.model,
                    intent.effort,
                    intent.tokens.input,
                    intent.tokens.output,
                    intent.tokens.cache_creation,
                    intent.tokens.cache_read,
                    i64::from(intent.tokens_known),
                    intent.cost_usd,
                    i64::from(intent.cost_known),
                    i64::from(intent.target_released),
                    marshal_skills_json(&skills),
                    intent.worker_name,
                ],
            )
            .map_err(|_| MetricsOwnerError::store("upsert phase"))?;
        // Skill invocations are derived from the same list. An empty list
        // writes no invocation rows and — critically — still wrote the phase.
        for skill in &intent.parsed_skills {
            self.connection
                .execute(
                    "INSERT INTO skill_invocations
                        (mission_id, phase, persona, skill_name, source, invoked_at)
                     SELECT ?1, ?2, ?3, ?4, ?5, ?6
                     WHERE NOT EXISTS (
                        SELECT 1 FROM skill_invocations
                        WHERE mission_id = ?1 AND phase = ?2 AND skill_name = ?4
                     )",
                    params![
                        intent.mission.as_str(),
                        intent.phase,
                        intent.persona,
                        skill.name,
                        "output_parse",
                        &intent.phase,
                    ],
                )
                .map_err(|_| MetricsOwnerError::store("insert skill invocation"))?;
        }
        Ok(())
    }

    /// Writes mission-level aggregates over phase rows that already exist.
    ///
    /// Counts are recomputed from `phases` rather than trusted from a caller,
    /// so a mission's totals cannot disagree with the phases it actually has.
    ///
    /// # Errors
    /// Returns [`MetricsOwnerError::Store`] when the upsert fails.
    pub fn record_terminal(&self, intent: &TerminalMetricIntent) -> Result<(), MetricsOwnerError> {
        self.capability.verify()?;
        self.connection
            .execute(
                "INSERT INTO missions (
                    id, domain, task, started_at, finished_at, duration_s, status,
                    decomp_source,
                    phases_total, phases_completed, phases_failed, phases_skipped,
                    retries_total, gate_failures,
                    tokens_in_total, tokens_out_total,
                    tokens_cache_creation_total, tokens_cache_read_total, tokens_known,
                    cost_usd_total, cost_known
                 )
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                    count(*),
                    coalesce(sum(status = 'completed'), 0),
                    coalesce(sum(status = 'failed'), 0),
                    coalesce(sum(status = 'skipped'), 0),
                    coalesce(sum(retries), 0),
                    coalesce(sum(gate_passed = 0), 0),
                    coalesce(sum(tokens_in), 0),
                    coalesce(sum(tokens_out), 0),
                    coalesce(sum(tokens_cache_creation), 0),
                    coalesce(sum(tokens_cache_read), 0),
                    coalesce(min(tokens_known), 0),
                    coalesce(sum(CASE WHEN cost_known = 1 THEN cost_usd ELSE 0 END), 0),
                    coalesce(min(cost_known), 0)
                 FROM phases WHERE mission_id = ?1
                 ON CONFLICT(id) DO UPDATE SET
                    domain = excluded.domain,
                    task = excluded.task,
                    started_at = excluded.started_at,
                    finished_at = excluded.finished_at,
                    duration_s = excluded.duration_s,
                    status = excluded.status,
                    decomp_source = excluded.decomp_source,
                    phases_total = excluded.phases_total,
                    phases_completed = excluded.phases_completed,
                    phases_failed = excluded.phases_failed,
                    phases_skipped = excluded.phases_skipped,
                    retries_total = excluded.retries_total,
                    gate_failures = excluded.gate_failures,
                    tokens_in_total = excluded.tokens_in_total,
                    tokens_out_total = excluded.tokens_out_total,
                    tokens_cache_creation_total = excluded.tokens_cache_creation_total,
                    tokens_cache_read_total = excluded.tokens_cache_read_total,
                    tokens_known = excluded.tokens_known,
                    cost_usd_total = excluded.cost_usd_total,
                    cost_known = excluded.cost_known",
                params![
                    intent.mission,
                    intent.domain,
                    intent.task,
                    intent.started_at,
                    intent.finished_at,
                    intent.duration_s,
                    intent.status,
                    if intent.decomp_source.is_empty() {
                        "unknown"
                    } else {
                        intent.decomp_source.as_str()
                    },
                ],
            )
            .map_err(|_| MetricsOwnerError::store("upsert mission"))?;
        Ok(())
    }

    /// Decodes one claimed publication and writes it.
    ///
    /// This is the drain consumer: [`crate::PublicationDrain`] hands it each
    /// claimed row, and its verdict decides whether the row resolves as
    /// delivered or as a retained dead letter.
    ///
    /// # Errors
    /// Returns [`MetricsOwnerError::UnknownIntent`] for a type this owner does
    /// not consume, [`MetricsOwnerError::MalformedIntent`] for an undecodable
    /// payload, and [`MetricsOwnerError::Store`] when the write fails.
    pub fn consume(&self, claimed: &ClaimedPublication) -> Result<(), MetricsOwnerError> {
        match claimed.type_name() {
            PHASE_METRIC_TYPE => {
                let intent = decode_phase_payload(claimed)?;
                self.record_phase(&intent)
            }
            TERMINAL_METRIC_TYPE => {
                let intent = decode_terminal_payload(claimed)?;
                self.record_terminal(&intent)
            }
            other => Err(MetricsOwnerError::UnknownIntent {
                type_name: other.to_owned(),
            }),
        }
    }

    /// Builds the bounded private projection through this owner's actual connection.
    pub(crate) fn private_run_metrics(
        &self,
        limit: usize,
    ) -> Result<Option<PrivateRunMetricsResult>, MetricsOwnerError> {
        self.capability.verify()?;
        OwnerConnectionMetricsQueryService {
            connection: &self.connection,
        }
        .query_private_run(PrivateRunMetricsQuery { limit })
        .map_err(MetricsOwnerError::from)
    }

    /// Reads back one phase row's `(status, gate_passed, parsed_skills)`.
    ///
    /// The owner is the writer, so recovery assertions read through it rather
    /// than opening a second connection to the same file.
    ///
    /// # Errors
    /// Returns [`MetricsOwnerError::Store`] when the read fails.
    pub fn phase_row(
        &self,
        mission: &str,
        phase: &str,
    ) -> Result<Option<RecordedPhase>, MetricsOwnerError> {
        self.capability.verify()?;
        let key = format!("{mission}_{phase}");
        let mut statement = self
            .connection
            .prepare(
                "SELECT status, gate_passed, parsed_skills, retries, error_message
                 FROM phases WHERE id = ?1",
            )
            .map_err(|_| MetricsOwnerError::store("prepare phase read"))?;
        let mut rows = statement
            .query(params![key])
            .map_err(|_| MetricsOwnerError::store("query phase read"))?;
        let Some(row) = rows
            .next()
            .map_err(|_| MetricsOwnerError::store("decode phase read"))?
        else {
            return Ok(None);
        };
        let skills_json: String = row
            .get(2)
            .map_err(|_| MetricsOwnerError::store("decode phase skills"))?;
        Ok(Some(RecordedPhase {
            status: row
                .get(0)
                .map_err(|_| MetricsOwnerError::store("decode phase status"))?,
            gate_passed: row
                .get::<_, i64>(1)
                .map_err(|_| MetricsOwnerError::store("decode phase gate"))?
                != 0,
            parsed_skills: unmarshal_skills_json(&skills_json),
            retries: row
                .get(3)
                .map_err(|_| MetricsOwnerError::store("decode phase retries"))?,
            error_message: row
                .get(4)
                .map_err(|_| MetricsOwnerError::store("decode phase error"))?,
        }))
    }

    /// Counts phase rows recorded for one mission, clamped to
    /// [`MAX_QUERY_LIMIT`].
    ///
    /// # Errors
    /// Returns [`MetricsOwnerError::Store`] when the count fails.
    pub fn recorded_phase_names(&self, mission: &str) -> Result<Vec<String>, MetricsOwnerError> {
        self.capability.verify()?;
        let limit = i64::try_from(MAX_QUERY_LIMIT).unwrap_or(i64::MAX);
        let mut statement = self
            .connection
            .prepare("SELECT name FROM phases WHERE mission_id = ?1 ORDER BY name LIMIT ?2")
            .map_err(|_| MetricsOwnerError::store("prepare phase name scan"))?;
        let rows = statement
            .query_map(params![mission, limit], |row| row.get::<_, String>(0))
            .map_err(|_| MetricsOwnerError::store("query phase name scan"))?;
        let mut names = Vec::new();
        for row in rows {
            names.push(row.map_err(|_| MetricsOwnerError::store("decode phase name"))?);
        }
        Ok(names)
    }

    /// Reads back one mission's recorded aggregates.
    ///
    /// # Errors
    /// Returns [`MetricsOwnerError::Store`] when the read fails.
    pub fn mission_totals(
        &self,
        mission: &str,
    ) -> Result<Option<MissionTotals>, MetricsOwnerError> {
        self.capability.verify()?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT status, duration_s, phases_total, phases_completed, phases_failed,
                        phases_skipped, retries_total, gate_failures, cost_usd_total
                 FROM missions WHERE id = ?1",
            )
            .map_err(|_| MetricsOwnerError::store("prepare mission read"))?;
        let mut rows = statement
            .query(params![mission])
            .map_err(|_| MetricsOwnerError::store("query mission read"))?;
        let Some(row) = rows
            .next()
            .map_err(|_| MetricsOwnerError::store("decode mission read"))?
        else {
            return Ok(None);
        };
        let read = |index: usize| -> Result<i64, MetricsOwnerError> {
            row.get(index)
                .map_err(|_| MetricsOwnerError::store("decode mission column"))
        };
        Ok(Some(MissionTotals {
            status: row
                .get(0)
                .map_err(|_| MetricsOwnerError::store("decode mission status"))?,
            duration_s: read(1)?,
            phases_total: read(2)?,
            phases_completed: read(3)?,
            phases_failed: read(4)?,
            phases_skipped: read(5)?,
            retries_total: read(6)?,
            gate_failures: read(7)?,
            cost_usd_total: row
                .get(8)
                .map_err(|_| MetricsOwnerError::store("decode mission cost"))?,
        }))
    }

    /// Creates the mission row a phase's foreign key needs, if it is missing.
    fn ensure_mission_stub(&self, mission: &str) -> Result<(), MetricsOwnerError> {
        self.connection
            .execute(
                "INSERT OR IGNORE INTO missions (id, started_at, finished_at, status)
                 VALUES (?1, '', '', 'running')",
                params![mission],
            )
            .map_err(|_| MetricsOwnerError::store("ensure mission stub"))?;
        Ok(())
    }

    /// Replaces every secret-shaped run in `text` before it reaches storage.
    ///
    /// `phases.error_message` is the one column that carries free text from a
    /// worker's stderr, so it is the one place a credential can reach
    /// `metrics.db`. Redaction happens here, on the write path, rather than at
    /// read time: a secret that is never stored cannot leak through a reader
    /// that forgets to filter.
    fn redact(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        self.redactor.redact_into(text, &mut out);
        if out.len() > MAX_ERROR_MESSAGE_BYTES {
            out.truncate(floor_char_boundary(&out, MAX_ERROR_MESSAGE_BYTES));
        }
        out
    }

    /// The storage actor this owner writes under.
    #[must_use]
    pub const fn actor(&self) -> &StorageActorAuthority {
        &self.actor
    }
}

struct OwnerConnectionMetricsQueryService<'connection> {
    connection: &'connection Connection,
}

impl crate::metrics_query::sealed::Sealed for OwnerConnectionMetricsQueryService<'_> {}

impl MetricsQueryService for OwnerConnectionMetricsQueryService<'_> {
    fn query_missions(
        &self,
        _query: crate::metrics_query::MissionsQuery,
    ) -> Result<crate::metrics_query::MissionsResult, MetricsQueryError> {
        Self::unavailable()
    }

    fn query_persona_metrics(
        &self,
        _query: crate::metrics_query::PersonaMetricsQuery,
    ) -> Result<crate::metrics_query::PersonaMetricsResult, MetricsQueryError> {
        Self::unavailable()
    }

    fn query_skill_usage(
        &self,
        _query: crate::metrics_query::SkillUsageQuery,
    ) -> Result<crate::metrics_query::SkillUsageResult, MetricsQueryError> {
        Self::unavailable()
    }

    fn query_trends(
        &self,
        _query: crate::metrics_query::TrendsQuery,
    ) -> Result<crate::metrics_query::TrendsResult, MetricsQueryError> {
        Self::unavailable()
    }

    fn query_routing_methods(
        &self,
        _query: crate::metrics_query::RoutingMethodsQuery,
    ) -> Result<crate::metrics_query::RoutingMethodsResult, MetricsQueryError> {
        Self::unavailable()
    }

    fn query_phases(
        &self,
        _query: crate::metrics_query::PhasesQuery,
    ) -> Result<crate::metrics_query::PhasesResult, MetricsQueryError> {
        Self::unavailable()
    }

    fn query_private_run(
        &self,
        query: PrivateRunMetricsQuery,
    ) -> Result<Option<PrivateRunMetricsResult>, MetricsQueryError> {
        crate::metrics_query::query_private_run_on_connection(self.connection, query)
    }
}

impl OwnerConnectionMetricsQueryService<'_> {
    fn unavailable<T>() -> Result<T, MetricsQueryError> {
        Err(MetricsQueryError::OwnerQueryNotEnrolled {
            protocol: crate::metrics_query::METRICS_OWNER_QUERY_PROTOCOL,
        })
    }
}

/// One phase row read back from `metrics.db`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedPhase {
    /// `phases.status`.
    pub status: String,
    /// `phases.gate_passed`, as a bool.
    pub gate_passed: bool,
    /// `phases.parsed_skills`, decoded. Empty for a skill-less phase.
    pub parsed_skills: Vec<String>,
    /// `phases.retries`.
    pub retries: i64,
    /// `phases.error_message`, already redacted at write time.
    pub error_message: String,
}

/// One mission row read back from `metrics.db`.
#[derive(Clone, Debug, PartialEq)]
pub struct MissionTotals {
    /// `missions.status`.
    pub status: String,
    /// `missions.duration_s`.
    pub duration_s: i64,
    /// `missions.phases_total`, recomputed from the phase rows.
    pub phases_total: i64,
    /// `missions.phases_completed`.
    pub phases_completed: i64,
    /// `missions.phases_failed`.
    pub phases_failed: i64,
    /// `missions.phases_skipped`.
    pub phases_skipped: i64,
    /// `missions.retries_total`.
    pub retries_total: i64,
    /// `missions.gate_failures`.
    pub gate_failures: i64,
    /// `missions.cost_usd_total`.
    pub cost_usd_total: f64,
}

/// Registered type name of a phase metric publication.
pub const PHASE_METRIC_TYPE: &str = "phase-metric";
/// Registered type name of a mission terminal metric publication.
pub const TERMINAL_METRIC_TYPE: &str = "mission-terminal-metric";

// ---------------------------------------------------------------------------
// Verification evidence (Gate 6)
// ---------------------------------------------------------------------------

/// A verification report in one of the three supported formats.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportFormat {
    /// The first-party helper control protocol on stderr.
    HelperProtocol,
    /// JUnit XML.
    JUnitXml,
    /// TAP version 13.
    Tap13,
}

/// Parses a JUnit XML report into a closed-schema summary.
///
/// Reads the aggregate attributes on the outermost `<testsuite>` or
/// `<testsuites>` element. A report whose attributes do not form a consistent
/// summary yields `None`, which classifies as `InfrastructureError` rather than
/// as any quality result — an unparseable report is not evidence of success.
#[must_use]
pub fn parse_junit_xml(text: &str) -> Option<VerificationSummary> {
    let element = find_element(text, "testsuites").or_else(|| find_element(text, "testsuite"))?;
    let tests = attribute(element, "tests")?;
    let failures = attribute(element, "failures").unwrap_or(0);
    let errors = attribute(element, "errors").unwrap_or(0);
    let skipped = attribute(element, "skipped").unwrap_or(0);
    let failed = failures.checked_add(errors)?;
    let executed = tests.checked_sub(skipped)?;
    let passed = executed.checked_sub(failed)?;
    Some(VerificationSummary {
        discovered: tests,
        executed,
        passed,
        failed,
        required_skipped: skipped,
    })
}

/// Parses a TAP 13 report into a closed-schema summary.
///
/// `# SKIP` and `# TODO` directives count as skipped, never as passed — this is
/// precisely the TRK-535 false-PASS family, where a skipped case was read as a
/// success because its line began with `ok`.
#[must_use]
pub fn parse_tap13(text: &str) -> Option<VerificationSummary> {
    let mut plan: Option<u64> = None;
    let mut passed = 0u64;
    let mut failed = 0u64;
    let mut skipped = 0u64;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("1..") {
            plan = rest.trim().parse::<u64>().ok();
            continue;
        }
        let (is_ok, body) = if let Some(rest) = line.strip_prefix("not ok") {
            (false, rest)
        } else if let Some(rest) = line.strip_prefix("ok") {
            (true, rest)
        } else {
            continue;
        };
        // "ok" must be a whole word: "okay 1 - ..." is not a result line.
        if !body.is_empty() && !body.starts_with(char::is_whitespace) {
            continue;
        }
        let directive = body
            .split_once('#')
            .map(|(_, directive)| directive.trim().to_ascii_uppercase());
        let is_skip = directive.is_some_and(|directive| {
            directive.starts_with("SKIP") || directive.starts_with("TODO")
        });
        if is_skip {
            skipped = skipped.saturating_add(1);
        } else if is_ok {
            passed = passed.saturating_add(1);
        } else {
            failed = failed.saturating_add(1);
        }
    }
    let seen = passed.checked_add(failed)?.checked_add(skipped)?;
    // A plan that disagrees with the emitted lines means the run did not finish
    // the suite it declared; that is an infrastructure fact, not a pass.
    let discovered = match plan {
        Some(planned) if planned != seen => return None,
        Some(planned) => planned,
        None => seen,
    };
    Some(VerificationSummary {
        discovered,
        executed: passed.checked_add(failed)?,
        passed,
        failed,
        required_skipped: skipped,
    })
}

/// Reconciles process facts against one or more parsed reports.
///
/// Unanimity is required: every source that produced a classification must
/// agree, and any disagreement — a helper exiting 0 while its JUnit report
/// records a failure, say — reconciles to `InfrastructureError`. Two sources
/// that contradict each other are not evidence for the more convenient one.
#[derive(Debug, Default)]
pub struct EvidenceReconciler {
    sources: Vec<(ReportFormat, VerificationOutcome)>,
}

impl EvidenceReconciler {
    /// An empty reconciler.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sources: Vec::new(),
        }
    }

    /// Adds one report's classification, derived from the same process
    /// termination every other source saw.
    #[must_use]
    pub fn with_report(
        mut self,
        format: ReportFormat,
        termination: VerificationTermination,
        summary: Option<VerificationSummary>,
    ) -> Self {
        self.sources
            .push((format, classify_verification(termination, summary)));
        self
    }

    /// The reconciled outcome.
    ///
    /// With no sources at all there is nothing to conclude, which is
    /// `InfrastructureError` — the absence of evidence is not evidence.
    #[must_use]
    pub fn reconcile(&self) -> VerificationOutcome {
        let mut distinct: Vec<VerificationOutcome> = Vec::new();
        for (_, outcome) in &self.sources {
            if !distinct.contains(outcome) {
                distinct.push(*outcome);
            }
        }
        match distinct.as_slice() {
            [single] => *single,
            _ => VerificationOutcome::Classified(VerificationClass::InfrastructureError),
        }
    }

    /// Per-source classifications, for diagnostics.
    #[must_use]
    pub fn sources(&self) -> &[(ReportFormat, VerificationOutcome)] {
        &self.sources
    }
}

/// Stable lowercase name for a class, used as `phases.error_type`.
#[must_use]
pub const fn verification_class_name(class: VerificationClass) -> &'static str {
    match class {
        VerificationClass::Pass => "pass",
        VerificationClass::Fail => "fail",
        VerificationClass::Skip => "skip",
        VerificationClass::NoTests => "no_tests",
        VerificationClass::Timeout => "timeout",
        VerificationClass::InfrastructureError => "infrastructure_error",
    }
}

// ---------------------------------------------------------------------------
// Go schema and payload codecs
// ---------------------------------------------------------------------------

/// The Go `metrics.db` schema, quoted from `internal/metrics/db.go:277-443`.
///
/// Written in the fresh `CREATE TABLE` form, with every migration-added column
/// already present, so a store this owner creates is byte-shaped exactly like
/// one the Go binary creates from scratch. `IF NOT EXISTS` throughout means
/// running it against an existing Go store is a no-op rather than an error.
const GO_METRICS_SCHEMA_DDL: &str = r"
CREATE TABLE IF NOT EXISTS missions (
    id                          TEXT PRIMARY KEY,
    domain                      TEXT NOT NULL DEFAULT '',
    task                        TEXT NOT NULL DEFAULT '',
    started_at                  DATETIME NOT NULL,
    finished_at                 DATETIME NOT NULL,
    duration_s                  INTEGER NOT NULL DEFAULT 0,
    phases_total                INTEGER NOT NULL DEFAULT 0,
    phases_completed            INTEGER NOT NULL DEFAULT 0,
    phases_failed               INTEGER NOT NULL DEFAULT 0,
    phases_skipped              INTEGER NOT NULL DEFAULT 0,
    learnings_retrieved         INTEGER NOT NULL DEFAULT 0,
    retries_total               INTEGER NOT NULL DEFAULT 0,
    gate_failures               INTEGER NOT NULL DEFAULT 0,
    output_len_total            INTEGER NOT NULL DEFAULT 0,
    status                      TEXT NOT NULL DEFAULT '',
    decomp_source               TEXT NOT NULL DEFAULT 'unknown',
    tokens_in_total             INTEGER NOT NULL DEFAULT 0,
    tokens_out_total            INTEGER NOT NULL DEFAULT 0,
    tokens_cache_creation_total INTEGER NOT NULL DEFAULT 0,
    tokens_cache_read_total     INTEGER NOT NULL DEFAULT 0,
    tokens_known                INTEGER NOT NULL DEFAULT 1,
    cost_usd_total              REAL NOT NULL DEFAULT 0,
    cost_known                  INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS phases (
    id                   TEXT PRIMARY KEY,
    mission_id           TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
    name                 TEXT NOT NULL DEFAULT '',
    persona              TEXT NOT NULL DEFAULT '',
    selection_method     TEXT NOT NULL DEFAULT '',
    duration_s           INTEGER NOT NULL DEFAULT 0,
    status               TEXT NOT NULL DEFAULT '',
    retries              INTEGER NOT NULL DEFAULT 0,
    gate_passed          INTEGER NOT NULL DEFAULT 0,
    output_len           INTEGER NOT NULL DEFAULT 0,
    learnings_retrieved  INTEGER NOT NULL DEFAULT 0,
    error_type           TEXT NOT NULL DEFAULT '',
    error_message        TEXT NOT NULL DEFAULT '',
    provider             TEXT NOT NULL DEFAULT '',
    model                TEXT NOT NULL DEFAULT '',
    effort               TEXT NOT NULL DEFAULT '',
    tokens_in            INTEGER NOT NULL DEFAULT 0,
    tokens_out           INTEGER NOT NULL DEFAULT 0,
    tokens_cache_creation INTEGER NOT NULL DEFAULT 0,
    tokens_cache_read    INTEGER NOT NULL DEFAULT 0,
    tokens_known         INTEGER NOT NULL DEFAULT 1,
    cost_usd             REAL NOT NULL DEFAULT 0,
    cost_known           INTEGER NOT NULL DEFAULT 1,
    target_released      INTEGER NOT NULL DEFAULT 0,
    parsed_skills        TEXT NOT NULL DEFAULT '',
    worker_name          TEXT NOT NULL DEFAULT '',
    barok_applied        INTEGER NOT NULL DEFAULT 0,
    barok_retry          INTEGER NOT NULL DEFAULT 0,
    barok_validator_ms   INTEGER NOT NULL DEFAULT 0,
    barok_retry_validator_ms INTEGER NOT NULL DEFAULT 0,
    output_bytes         INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS skill_invocations (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
    phase      TEXT NOT NULL DEFAULT '',
    persona    TEXT NOT NULL DEFAULT '',
    skill_name TEXT NOT NULL DEFAULT '',
    source     TEXT NOT NULL DEFAULT 'declared',
    invoked_at DATETIME NOT NULL
);
CREATE TABLE IF NOT EXISTS quota_snapshots (
    id                       INTEGER PRIMARY KEY AUTOINCREMENT,
    captured_at              DATETIME NOT NULL,
    mission_id               TEXT NOT NULL,
    tokens_in                INTEGER NOT NULL DEFAULT 0,
    tokens_out               INTEGER NOT NULL DEFAULT 0,
    tokens_cache_read        INTEGER NOT NULL DEFAULT 0,
    cost_usd                 REAL NOT NULL DEFAULT 0,
    window_5h_tokens_in      INTEGER NOT NULL DEFAULT 0,
    window_5h_tokens_out     INTEGER NOT NULL DEFAULT 0,
    window_5h_cost_usd       REAL NOT NULL DEFAULT 0,
    estimated_5h_utilization REAL NOT NULL DEFAULT 0,
    model                    TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS usage_snapshots (
    id                         INTEGER PRIMARY KEY AUTOINCREMENT,
    captured_at                DATETIME NOT NULL,
    mission_id                 TEXT NOT NULL DEFAULT '',
    five_hour_util             REAL NOT NULL DEFAULT 0,
    five_hour_resets_at        DATETIME,
    seven_day_util             REAL NOT NULL DEFAULT 0,
    seven_day_resets_at        DATETIME,
    seven_day_sonnet_util      REAL NOT NULL DEFAULT 0,
    seven_day_sonnet_resets_at DATETIME,
    raw_json                   TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_missions_domain ON missions(domain);
CREATE INDEX IF NOT EXISTS idx_missions_status ON missions(status);
CREATE INDEX IF NOT EXISTS idx_missions_started_at ON missions(started_at);
CREATE INDEX IF NOT EXISTS idx_phases_mission_id ON phases(mission_id);
CREATE INDEX IF NOT EXISTS idx_phases_persona ON phases(persona);
CREATE INDEX IF NOT EXISTS idx_skill_invocations_mission_id ON skill_invocations(mission_id);
CREATE INDEX IF NOT EXISTS idx_skill_invocations_skill_name ON skill_invocations(skill_name);
CREATE INDEX IF NOT EXISTS idx_skill_invocations_persona ON skill_invocations(persona);
CREATE INDEX IF NOT EXISTS idx_skill_invocations_persona_skill
    ON skill_invocations(persona, skill_name);
";

/// Indexes that must be created *after* the additive migrations
/// (`internal/metrics/db.go:415-427`).
///
/// Go separates these for a concrete reason, restated in its own source: each
/// one names a column that a pre-migration database does not yet have. Creating
/// `idx_phases_worker_name` before `ALTER TABLE phases ADD COLUMN worker_name`
/// fails outright, so the ordering is load-bearing rather than stylistic.
const GO_METRICS_POST_MIGRATION_INDEXES: &str = r"
CREATE INDEX IF NOT EXISTS idx_skill_invocations_phase ON skill_invocations(phase);
CREATE INDEX IF NOT EXISTS idx_quota_snapshots_captured_at ON quota_snapshots(captured_at);
CREATE INDEX IF NOT EXISTS idx_quota_snapshots_mission_id ON quota_snapshots(mission_id);
CREATE INDEX IF NOT EXISTS idx_phases_worker_name ON phases(worker_name);
CREATE INDEX IF NOT EXISTS idx_usage_snapshots_captured_at ON usage_snapshots(captured_at);
";

fn open_metrics_db(path: &Path) -> Result<Connection, MetricsOwnerError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_FULL_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| MetricsOwnerError::store("open metrics.db"))?;
    // The same pragmas Go sets at `db.go:232-236`, so the two runtimes agree on
    // journal mode and foreign-key enforcement over the same file.
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;
             PRAGMA foreign_keys=ON;",
        )
        .map_err(|_| MetricsOwnerError::store("configure metrics.db"))?;
    Ok(connection)
}

/// Go's additive `ALTER TABLE` migrations (`internal/metrics/db.go:352-405`).
///
/// A store created by an older Go binary has the tables but not every column.
/// Go applies these on open and suppresses "duplicate column name"; this owner
/// must do the same, or an upsert naming `tokens_cache_creation` would fail
/// against a database the Go binary reads perfectly well.
const GO_METRICS_ADDITIVE_MIGRATIONS: &[&str] = &[
    "ALTER TABLE phases ADD COLUMN selection_method TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE phases ADD COLUMN provider TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE phases ADD COLUMN model TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE phases ADD COLUMN tokens_in INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE phases ADD COLUMN tokens_out INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE phases ADD COLUMN cost_usd REAL NOT NULL DEFAULT 0",
    "ALTER TABLE missions ADD COLUMN tokens_in_total INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE missions ADD COLUMN tokens_out_total INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE missions ADD COLUMN cost_usd_total REAL NOT NULL DEFAULT 0",
    "ALTER TABLE skill_invocations ADD COLUMN phase TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE skill_invocations ADD COLUMN source TEXT NOT NULL DEFAULT 'declared'",
    "ALTER TABLE missions ADD COLUMN decomp_source TEXT NOT NULL DEFAULT 'unknown'",
    "ALTER TABLE phases ADD COLUMN parsed_skills TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE phases ADD COLUMN tokens_cache_creation INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE phases ADD COLUMN tokens_cache_read INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE missions ADD COLUMN tokens_cache_creation_total INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE missions ADD COLUMN tokens_cache_read_total INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE phases ADD COLUMN error_type TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE phases ADD COLUMN error_message TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE phases ADD COLUMN worker_name TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE phases ADD COLUMN barok_applied INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE phases ADD COLUMN barok_retry INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE phases ADD COLUMN barok_validator_ms INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE phases ADD COLUMN barok_retry_validator_ms INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE phases ADD COLUMN output_bytes INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE phases ADD COLUMN effort TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE phases ADD COLUMN tokens_known INTEGER NOT NULL DEFAULT 1",
    "ALTER TABLE phases ADD COLUMN cost_known INTEGER NOT NULL DEFAULT 1",
    "ALTER TABLE phases ADD COLUMN target_released INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE missions ADD COLUMN tokens_known INTEGER NOT NULL DEFAULT 1",
    "ALTER TABLE missions ADD COLUMN cost_known INTEGER NOT NULL DEFAULT 1",
];

fn apply_go_schema(connection: &Connection) -> Result<(), MetricsOwnerError> {
    connection
        .execute_batch(GO_METRICS_SCHEMA_DDL)
        .map_err(|_| MetricsOwnerError::store("apply metrics schema"))?;
    for migration in GO_METRICS_ADDITIVE_MIGRATIONS {
        match connection.execute(migration, []) {
            Ok(_) => {}
            // Only "duplicate column name" is suppressed, exactly as Go does at
            // `db.go:408-412`. Any other failure is a real error rather than
            // something to run past.
            Err(error) if is_duplicate_column(&error) => {}
            Err(_) => return Err(MetricsOwnerError::store("apply metrics migration")),
        }
    }
    connection
        .execute_batch(GO_METRICS_POST_MIGRATION_INDEXES)
        .map_err(|_| MetricsOwnerError::store("apply metrics post-migration indexes"))?;
    Ok(())
}

fn is_duplicate_column(error: &rusqlite::Error) -> bool {
    error.to_string().contains("duplicate column name")
}

/// Mirrors Go's `marshalSkillsJSON` (`db.go:589-598`): `""` for an empty slice.
fn marshal_skills_json(skills: &[&str]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    serde_json::to_string(skills).unwrap_or_default()
}

/// Mirrors Go's `unmarshalSkillsJSON` (`db.go:602-611`).
fn unmarshal_skills_json(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    serde_json::from_str(text).unwrap_or_default()
}

fn decode_phase_payload(
    claimed: &ClaimedPublication,
) -> Result<PhaseMetricIntent, MetricsOwnerError> {
    let value: serde_json::Value = serde_json::from_str(claimed.payload_json()).map_err(|_| {
        MetricsOwnerError::MalformedIntent {
            context: Redacted::new("phase metric payload is not JSON".to_owned()),
        }
    })?;
    let map = value
        .as_object()
        .ok_or_else(|| MetricsOwnerError::MalformedIntent {
            context: Redacted::new("phase metric payload is not an object".to_owned()),
        })?;
    let intent = PhaseMetricIntentPayload::from_map(map)?;
    intent.into_intent(claimed)
}

fn decode_terminal_payload(
    claimed: &ClaimedPublication,
) -> Result<TerminalMetricIntent, MetricsOwnerError> {
    serde_json::from_str::<TerminalMetricIntentPayload>(claimed.payload_json())
        .map(|payload| TerminalMetricIntent {
            mission: claimed.mission_id().as_str().to_owned(),
            domain: payload.domain,
            task: payload.task,
            started_at: payload.started_at,
            finished_at: payload.finished_at,
            duration_s: payload.duration_s,
            status: payload.status,
            decomp_source: payload.decomp_source,
        })
        .map_err(|_| MetricsOwnerError::MalformedIntent {
            context: Redacted::new("terminal metric payload is undecodable".to_owned()),
        })
}

#[derive(serde::Deserialize)]
struct TerminalMetricIntentPayload {
    #[serde(default)]
    domain: String,
    #[serde(default)]
    task: String,
    #[serde(default)]
    started_at: String,
    #[serde(default)]
    finished_at: String,
    #[serde(default)]
    duration_s: i64,
    #[serde(default)]
    status: String,
    #[serde(default)]
    decomp_source: String,
}

/// The wire shape of an enqueued phase metric.
///
/// Deliberately *not* `Deserialize` for [`PhaseMetricIntent`] itself: the
/// reaping witness cannot be serialized, and making it round-trip would mean
/// accepting a caller's word for it. The witness is instead re-proved on the
/// drain side by [`PhaseMetricIntentPayload::into_intent`], which requires the
/// publication's own recorded process identity.
struct PhaseMetricIntentPayload {
    persona: String,
    selection_method: String,
    status: String,
    duration_s: i64,
    retries: i64,
    gate_passed: bool,
    provider: String,
    model: String,
    effort: String,
    tokens: TokenCounts,
    tokens_known: bool,
    cost_usd: f64,
    cost_known: bool,
    target_released: bool,
    worker_name: String,
    parsed_skills: Vec<String>,
    error_type: String,
    error_message: String,
    reaped_pid: u32,
    reaped_process_group_id: u32,
    reaped_start_identity: String,
}

impl PhaseMetricIntentPayload {
    fn from_map(
        map: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, MetricsOwnerError> {
        let text = |key: &str| -> String {
            map.get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let number = |key: &str| -> i64 {
            map.get(key)
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default()
        };
        let identifier = |key: &str| -> Result<u32, MetricsOwnerError> {
            map.get(key)
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| MetricsOwnerError::MalformedIntent {
                    context: Redacted::new("phase metric reaping witness is absent".to_owned()),
                })
        };
        Ok(Self {
            persona: text("persona"),
            selection_method: text("selection_method"),
            status: text("status"),
            duration_s: number("duration_s"),
            retries: number("retries"),
            gate_passed: map
                .get("gate_passed")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or_default(),
            provider: text("provider"),
            model: text("model"),
            effort: text("effort"),
            tokens: TokenCounts {
                input: number("tokens_in"),
                output: number("tokens_out"),
                cache_creation: number("tokens_cache_creation"),
                cache_read: number("tokens_cache_read"),
            },
            tokens_known: map
                .get("tokens_known")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            cost_usd: map
                .get("cost_usd")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or_default(),
            cost_known: map
                .get("cost_known")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            target_released: map
                .get("target_released")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or_default(),
            worker_name: text("worker_name"),
            parsed_skills: map
                .get("parsed_skills")
                .and_then(serde_json::Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            error_type: text("error_type"),
            error_message: text("error_message"),
            reaped_pid: identifier("reaped_pid")?,
            reaped_process_group_id: identifier("reaped_process_group_id")?,
            reaped_start_identity: text("reaped_start_identity"),
        })
    }

    fn into_intent(
        self,
        claimed: &ClaimedPublication,
    ) -> Result<PhaseMetricIntent, MetricsOwnerError> {
        let phase = claimed.phase_id().unwrap_or_default().to_owned();
        // Re-prove absence from the persisted identity rather than trusting the
        // enqueue-time verdict: between commit and drain the world moved on,
        // and a group that is somehow live again must not be recorded as reaped.
        let status = orchestrator_process::inspect_recorded_process_identity(
            self.reaped_pid,
            self.reaped_process_group_id,
            &self.reaped_start_identity,
        )
        .map_err(|_| MetricsOwnerError::ProcessGroupNotReaped {
            phase: phase.clone(),
        })?;
        let orchestrator_process::RecordedProcessIdentityStatus::ExactGroupAbsent(reaping) = status
        else {
            return Err(MetricsOwnerError::ProcessGroupNotReaped { phase });
        };
        let mut intent = PhaseMetricIntent::new(
            claimed.mission_id().clone(),
            phase,
            claimed.logical_attempt(),
            reaping,
        );
        intent.persona = self.persona;
        intent.selection_method = self.selection_method;
        intent.status = match self.status.as_str() {
            "completed" => PhaseStatus::Completed,
            "skipped" => PhaseStatus::Skipped,
            _ => PhaseStatus::Failed,
        };
        intent.duration_s = self.duration_s;
        intent.retries = self.retries;
        // A recorded gate pass must agree with the recorded status. Anything
        // else is contradictory evidence, and contradictory evidence is not a
        // pass.
        intent.gate_passed = self.gate_passed && matches!(intent.status, PhaseStatus::Completed);
        intent.provider = self.provider;
        intent.model = self.model;
        intent.effort = self.effort;
        intent.tokens = self.tokens;
        intent.tokens_known = self.tokens_known;
        intent.cost_usd = self.cost_usd;
        intent.cost_known = self.cost_known;
        intent.target_released = self.target_released;
        intent.worker_name = self.worker_name;
        intent.parsed_skills = self
            .parsed_skills
            .into_iter()
            .map(|name| SkillRef {
                name,
                ..SkillRef::default()
            })
            .collect();
        intent.error_type = self.error_type;
        intent.error_message = self.error_message;
        Ok(intent)
    }
}

// ---------------------------------------------------------------------------
// Small parsing helpers
// ---------------------------------------------------------------------------

/// Finds `<name ...>` and returns the attribute text inside the tag.
///
/// A deliberately small reader rather than an XML dependency: the gate needs
/// exactly the aggregate attributes on one element, and a full parser would add
/// a supply-chain edge for four integers.
fn find_element<'text>(text: &'text str, name: &str) -> Option<&'text str> {
    let needle = format!("<{name}");
    let start = text.find(&needle)?;
    let after = &text[start + needle.len()..];
    // Reject `<testsuites2 ...>` matching a search for `testsuites`.
    if !after
        .chars()
        .next()
        .is_some_and(|next| next.is_whitespace() || next == '>' || next == '/')
    {
        return None;
    }
    let end = after.find('>')?;
    Some(&after[..end])
}

/// Reads one whole-name attribute out of an element's attribute text.
///
/// Walks `element` token by token — `name="value"` — rather than searching
/// for the substring `name="` anywhere in the text. A substring search finds
/// a match anywhere, including inside another attribute's own quoted value
/// (e.g. a `message="... failures=\"7\" ..."` value containing that literal
/// text); a token-by-token walk consumes each attribute's value in full
/// before looking for the next attribute name, so a decoy sitting inside an
/// already-consumed value is never independently searched. It also keeps the
/// whole-word guard this replaced: `disabled-failures="0"` is a different
/// token than `failures`, so it cannot answer for a later `failures="7"` —
/// under-reading the failure count would let `passed` absorb the difference
/// and a failing suite classify as a pass (the TRK-535 false-PASS family).
/// This is the guard [`find_element`] already applies to element names.
fn attribute(element: &str, name: &str) -> Option<u64> {
    let mut rest = element;
    loop {
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            return None;
        }
        let token_end = trimmed.find(|c: char| c == '=' || c.is_whitespace())?;
        let token = &trimmed[..token_end];
        let after_token = trimmed[token_end..].trim_start();
        let Some(after_eq) = after_token.strip_prefix('=') else {
            // This token was never followed by `=`; it is text inside a
            // value we already consumed (or otherwise not an attribute).
            // Resync on it and keep scanning rather than giving up.
            rest = after_token;
            continue;
        };
        let after_eq = after_eq.trim_start();
        let Some(value_rest) = after_eq.strip_prefix('"') else {
            // `=` not followed by a quoted value: skip past it and resync.
            rest = after_eq;
            continue;
        };
        let Some(end) = value_rest.find('"') else {
            return None; // unterminated quoted value; nothing further to read
        };
        if token == name {
            return value_rest[..end].trim().parse().ok();
        }
        rest = &value_rest[end + 1..];
    }
}

/// `str::floor_char_boundary` is unstable; this is the same walk.
fn floor_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    let mut candidate = index;
    while candidate > 0 && !text.is_char_boundary(candidate) {
        candidate -= 1;
    }
    candidate
}

/// Renders a phase metric as the canonical JSON body of a publication.
///
/// The reaping witness travels as its three persisted identity fields, and the
/// drain re-proves absence from them rather than trusting a boolean.
#[must_use]
pub fn phase_metric_payload(intent: &PhaseMetricIntent) -> serde_json::Value {
    let skills: Vec<&str> = intent
        .parsed_skills
        .iter()
        .map(|skill| skill.name.as_str())
        .collect();
    serde_json::json!({
        "cost_usd": intent.cost_usd,
        "duration_s": intent.duration_s,
        "error_message": intent.error_message,
        "error_type": intent.error_type,
        "gate_passed": intent.gate_passed,
        "model": intent.model,
        "effort": intent.effort,
        "parsed_skills": skills,
        "persona": intent.persona,
        "provider": intent.provider,
        "reaped_pid": intent.reaping.pid(),
        "reaped_process_group_id": intent.reaping.process_group_id(),
        "reaped_start_identity": intent.reaping.process_start_identity(),
        "retries": intent.retries,
        "selection_method": intent.selection_method,
        "status": intent.status.as_str(),
        "tokens_cache_creation": intent.tokens.cache_creation,
        "tokens_cache_read": intent.tokens.cache_read,
        "tokens_known": intent.tokens_known,
        "tokens_in": intent.tokens.input,
        "tokens_out": intent.tokens.output,
        "cost_known": intent.cost_known,
        "target_released": intent.target_released,
        "worker_name": intent.worker_name,
    })
}

/// Renders a mission terminal metric as the canonical JSON body.
#[must_use]
pub fn terminal_metric_payload(intent: &TerminalMetricIntent) -> serde_json::Value {
    serde_json::json!({
        "decomp_source": intent.decomp_source,
        "domain": intent.domain,
        "duration_s": intent.duration_s,
        "finished_at": intent.finished_at,
        "started_at": intent.started_at,
        "status": intent.status,
        "task": intent.task,
    })
}

/// Clamps any caller-supplied page size to [`MAX_QUERY_LIMIT`].
///
/// Zero and negative values keep Go's documented default rather than becoming
/// an unbounded scan.
#[must_use]
pub const fn clamp_limit(requested: i64, default: i64) -> i64 {
    let effective = if requested <= 0 { default } else { requested };
    let ceiling = MAX_QUERY_LIMIT as i64;
    if effective > ceiling {
        ceiling
    } else {
        effective
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junit_summary_separates_skipped_from_passed() -> Result<(), Box<dyn std::error::Error>> {
        let summary = parse_junit_xml(
            r#"<testsuites name="s" tests="5" failures="0" errors="0" skipped="2"></testsuites>"#,
        )
        .ok_or("a well-formed JUnit aggregate must parse")?;
        assert_eq!(summary.discovered, 5);
        assert_eq!(summary.executed, 3);
        assert_eq!(summary.passed, 3);
        assert_eq!(summary.required_skipped, 2);
        assert_eq!(
            classify_verification(VerificationTermination::Exited(0), Some(summary)),
            VerificationOutcome::Classified(VerificationClass::Skip),
        );
        Ok(())
    }

    #[test]
    fn junit_element_match_is_whole_word() {
        assert!(parse_junit_xml(r#"<testsuitesx tests="1"></testsuitesx>"#).is_none());
    }

    #[test]
    fn tap_skip_directive_never_counts_as_passed() -> Result<(), Box<dyn std::error::Error>> {
        let summary = parse_tap13("1..3\nok 1 - a\nok 2 - b # SKIP not applicable\nok 3 - c")
            .ok_or("a well-formed TAP plan must parse")?;
        assert_eq!(summary.passed, 2);
        assert_eq!(summary.required_skipped, 1);
        assert_eq!(
            classify_verification(VerificationTermination::Exited(0), Some(summary)),
            VerificationOutcome::Classified(VerificationClass::Skip),
        );
        Ok(())
    }

    #[test]
    fn tap_plan_disagreement_is_not_a_summary() -> Result<(), Box<dyn std::error::Error>> {
        assert!(parse_tap13("1..5\nok 1 - a").is_none());
        // "okay" is not a result line.
        let summary = parse_tap13("1..1\nokay 1 - a\nok 1 - b").ok_or("one real result line")?;
        assert_eq!(summary.passed, 1);
        Ok(())
    }

    #[test]
    fn empty_suite_classifies_as_no_tests() -> Result<(), Box<dyn std::error::Error>> {
        let summary = parse_junit_xml(r#"<testsuite tests="0" failures="0"></testsuite>"#)
            .ok_or("an empty suite must still parse")?;
        assert_eq!(
            classify_verification(VerificationTermination::Exited(0), Some(summary)),
            VerificationOutcome::Classified(VerificationClass::NoTests),
        );
        Ok(())
    }

    #[test]
    fn contradictory_sources_reconcile_to_infrastructure_error() {
        // Helper exits 0 and reports all-pass; its JUnit report records a
        // failure. Neither wins.
        let reconciler = EvidenceReconciler::new()
            .with_report(
                ReportFormat::HelperProtocol,
                VerificationTermination::Exited(0),
                Some(VerificationSummary {
                    discovered: 2,
                    executed: 2,
                    passed: 2,
                    failed: 0,
                    required_skipped: 0,
                }),
            )
            .with_report(
                ReportFormat::JUnitXml,
                VerificationTermination::Exited(0),
                parse_junit_xml(r#"<testsuite tests="2" failures="1"></testsuite>"#),
            );
        let outcome = reconciler.reconcile();
        assert_eq!(
            outcome,
            VerificationOutcome::Classified(VerificationClass::InfrastructureError),
        );
        assert!(!outcome.gate_passed());
    }

    #[test]
    fn no_evidence_is_not_a_pass() {
        assert!(!EvidenceReconciler::new().reconcile().gate_passed());
    }

    #[test]
    fn clamp_limit_bounds_every_request() {
        assert_eq!(clamp_limit(0, DEFAULT_MISSIONS_LIMIT), 20);
        assert_eq!(clamp_limit(-5, DEFAULT_MISSIONS_LIMIT), 20);
        assert_eq!(clamp_limit(7, DEFAULT_MISSIONS_LIMIT), 7);
        assert_eq!(clamp_limit(i64::MAX, DEFAULT_MISSIONS_LIMIT), 1_000);
    }

    #[test]
    fn skills_json_matches_go_empty_encoding() {
        assert_eq!(marshal_skills_json(&[]), "");
        assert_eq!(marshal_skills_json(&["a", "b"]), r#"["a","b"]"#);
        assert!(unmarshal_skills_json("").is_empty());
        assert_eq!(unmarshal_skills_json(r#"["a"]"#), vec!["a".to_owned()]);
    }
}
