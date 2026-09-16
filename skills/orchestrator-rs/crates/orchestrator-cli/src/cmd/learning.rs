//! Ports the flag surface and the dry-run/`--apply` contract of the Go
//! orchestrator's learning-maintenance commands: `stats`, `prune`, and
//! `backfill-embeddings` (`internal/cmd/learn.go`) and `archive`
//! (`internal/cmd/archive.go`).
//!
//! Every store access goes through an injected [`GoLearningReader`], which is
//! read-only by construction — the trait carries no update, delete, or DDL
//! method, so nothing in this module can express a mutation. There is no
//! filesystem or SQLite capability here and no way to turn a `--db` path into
//! a store handle: B3-DESIGN §6 keeps `learnings.db` reachable only through an
//! authority.
//!
//! **The `--apply` half is deliberately not ported in B3.** Go's `--apply`
//! paths call `Cleanup` (DELETE), `ArchiveDeadWeight` (UPDATE `archived`),
//! `UpdateQualityScores` (UPDATE `quality_score`), and `SetEmbedding` (UPDATE
//! `embedding`). B3-DESIGN §3.1 declines to port all four by name, because the
//! Go writer still owns those rows and Addendum §K4 forbids two editable
//! authorities over one partition — the constraint Risk 4 calls "the single
//! most important operational constraint in this design". The one declared
//! additive edge into `learnings.db` is `GoLearningAppender::insert_new`, and
//! no `learn.go`/`archive.go` command carries it. So `--apply` is refused here
//! *before any store is opened*, rather than being silently downgraded to a
//! dry-run or hand-rolled as a second writer inside the CLI crate.
//!
//! **The dry-run data lines are reproduced from the widened read-only row**
//! (TRK-1248). [`orchestrator_app::LearningRow`] now also carries
//! `seen_count`, `used_count`, `injection_count`, `compliance_rate`, and
//! `has_embedding`, and [`orchestrator_app::LearningStats`] carries
//! `with_embeddings` plus the compliance aggregates — every column Go's
//! `showStats`, `Cleanup`, `ArchiveDeadWeight`, and `CountEmbeddingBackfill`
//! predicates read. All five additions are read-only: the trait still has no
//! update method and `NewLearningRow` still cannot set any of them, so the
//! Risk 4 single-writer constraint is untouched.
//!
//! Two divergences are deliberate and bounded:
//!
//! * **Row order.** Go's `ArchiveDeadWeight` criteria carry no `ORDER BY`, so
//!   its candidate list comes out in `rowid` order. The ported reader does not
//!   expose `rowid`, so candidates are ordered by `(created_at, id)` — which
//!   agrees with `rowid` for any store Go wrote, because every production
//!   capture path sets `CreatedAt: time.Now()` at insert
//!   (`internal/learning/capture.go`, `docs_ingest.go`) and SQLite appends
//!   rowids.
//! * **Page bound.** Go's counts are SQL aggregates over the whole table; this
//!   port counts ported rows and is therefore capped at
//!   [`MAX_ADAPTER_ROWS`]. A page that comes back truncated would produce an
//!   undercount, so it returns [`LearningError::StoreTooLarge`] instead of a
//!   plausible wrong number.

use std::{
    io::Write,
    time::{SystemTime, UNIX_EPOCH},
};

use orchestrator_app::{
    GoAdapterError, GoLearningReader, LearningListQuery, LearningRow, MAX_ADAPTER_ROWS,
};
use thiserror::Error;

use super::{
    CmdError, next_string_value, parse_bool_flag_value, parse_go_int64, split_flag,
    time_fmt::{format_unix_seconds_rfc3339, format_unix_seconds_ymd_hms},
};

/// Go's `embeddingPricePerMTokens` (`internal/cmd/learn.go:18`).
const EMBEDDING_PRICE_PER_M_TOKENS: f64 = 0.15;

/// Go's `embeddingCharsPerTokenEst` (`internal/cmd/learn.go:19`).
const EMBEDDING_CHARS_PER_TOKEN_EST: f64 = 4.0;

/// Failures for the `stats`/`prune`/`archive`/`backfill-embeddings` group.
#[derive(Debug, Error)]
pub(crate) enum LearningError {
    /// `--apply` was requested for a write B3 does not port.
    ///
    /// Raised before any store is opened, so a refused `--apply` cannot even
    /// read, let alone write.
    #[error(
        "{command} --apply is not ported: it mutates rows the Go writer still owns \
         ({operations}). B3-DESIGN §3.1 declines to port those writes and Risk 4 forbids a \
         second editable authority over learnings.db while the Go orchestrator can still \
         write it — run the accepted Go binary for this, or wait for the B5 fence-and-grant \
         step"
    )]
    ApplyNotPorted {
        /// The Go command name.
        command: &'static str,
        /// The Go write operations the flag would reach.
        operations: &'static str,
    },
    /// The store holds more rows than one bounded page can carry.
    ///
    /// Go computes these counts as SQL aggregates over the whole table; this
    /// port counts ported rows, so a truncated page would silently undercount.
    #[error(
        "{command} cannot be rendered: the store holds more than {limit} learnings, and this \
         port counts ported rows rather than aggregating in SQL, so the number would be an \
         undercount — run the accepted Go binary for this store"
    )]
    StoreTooLarge {
        /// The Go command name.
        command: &'static str,
        /// The per-page bound that was hit.
        limit: usize,
    },
    /// Go exits 2 for a batch size outside `1..=100`.
    #[error("invalid --batch-size {value} (must be 1..100)")]
    InvalidBatchSize {
        /// The rejected value.
        value: i64,
    },
    /// Go exits 2 for a negative `--since`.
    #[error("invalid --since {value} (must be >= 0)")]
    InvalidSince {
        /// The rejected literal, echoed as Go echoes it.
        value: String,
    },
    /// The injected reader failed.
    #[error("reading the learning store: {0}")]
    Read(#[source] GoAdapterError),
    /// Writing command output failed.
    #[error("cannot write command output: {0}")]
    Output(#[from] std::io::Error),
}

/// Which learning-maintenance command to run, with its own flags.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LearningCommand {
    /// `orchestrator stats`.
    Stats,
    /// `orchestrator prune`.
    Prune(PruneFlags),
    /// `orchestrator archive`.
    Archive(ArchiveFlags),
    /// `orchestrator backfill-embeddings`.
    BackfillEmbeddings(BackfillFlags),
}

impl LearningCommand {
    /// The Go command name, used in error text and by the gate.
    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::Stats => "stats",
            Self::Prune(_) => "prune",
            Self::Archive(_) => "archive",
            Self::BackfillEmbeddings(_) => "backfill-embeddings",
        }
    }

    /// Whether the invocation asked to write. Every command in this family is
    /// dry-run unless `--apply` is present, matching Go.
    pub(crate) const fn is_apply(&self) -> bool {
        match self {
            Self::Stats => false,
            Self::Prune(flags) => flags.apply,
            Self::Archive(flags) => flags.apply,
            Self::BackfillEmbeddings(flags) => flags.apply,
        }
    }
}

/// `prune` flags, with Go's defaults (`learn.go:35-38`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PruneFlags {
    pub(crate) apply: bool,
    pub(crate) max_age: i64,
    pub(crate) min_score: f64,
    pub(crate) max_count: i64,
}

impl Default for PruneFlags {
    fn default() -> Self {
        Self {
            apply: false,
            max_age: 180,
            min_score: 0.1,
            max_count: 500,
        }
    }
}

/// `archive` flags, with Go's defaults (`archive.go:26-29`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ArchiveFlags {
    pub(crate) apply: bool,
    pub(crate) domain: String,
    /// Go registers a hidden `--db`. It is parsed for flag-surface parity and
    /// then deliberately ignored: B3-DESIGN §6 forbids turning a caller-supplied
    /// path into a store handle, so the injected reader always wins.
    pub(crate) db: String,
}

/// `backfill-embeddings` flags, with Go's defaults (`learn.go:46-58`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BackfillFlags {
    pub(crate) apply: bool,
    pub(crate) since: GoDuration,
    pub(crate) limit: i64,
    pub(crate) batch_size: i64,
    pub(crate) rpm: i64,
    pub(crate) max_retries: i64,
    /// Parsed for parity, ignored for authority. See [`ArchiveFlags::db`].
    pub(crate) db: String,
    pub(crate) include_archived: bool,
    pub(crate) quiet: bool,
}

impl Default for BackfillFlags {
    fn default() -> Self {
        Self {
            apply: false,
            since: GoDuration::ZERO,
            limit: 0,
            batch_size: 100,
            rpm: 60,
            max_retries: 5,
            db: String::new(),
            include_archived: false,
            quiet: false,
        }
    }
}

/// A `time.Duration` value in nanoseconds, carrying the literal Go printed it
/// from so a rejection can echo the operator's own text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GoDuration {
    nanos: i64,
    literal: String,
}

impl GoDuration {
    const ZERO: Self = Self {
        nanos: 0,
        literal: String::new(),
    };

    pub(crate) const fn nanos(&self) -> i64 {
        self.nanos
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Ports the argument shape of the four Go commands. Each is a root-level
/// cobra command, so there is no parent/leaf split to mirror here.
pub(crate) fn parse(command: &str, arguments: &[String]) -> Result<LearningCommand, CmdError> {
    match command {
        "stats" => {
            reject_flags(arguments)?;
            Ok(LearningCommand::Stats)
        }
        "prune" => parse_prune(arguments).map(LearningCommand::Prune),
        "archive" => parse_archive(arguments).map(LearningCommand::Archive),
        "backfill-embeddings" => parse_backfill(arguments).map(LearningCommand::BackfillEmbeddings),
        other => Err(CmdError::UnknownLearningSubcommand(other.to_owned())),
    }
}

/// Go's `statsCmd` registers no flags, so cobra rejects any flag but accepts
/// (and ignores) positional arguments.
fn reject_flags(arguments: &[String]) -> Result<(), CmdError> {
    for argument in arguments {
        if argument.starts_with('-') && argument != "-" {
            return Err(CmdError::UnknownFlag(argument.clone()));
        }
    }
    Ok(())
}

fn parse_prune(arguments: &[String]) -> Result<PruneFlags, CmdError> {
    let mut flags = PruneFlags::default();
    let mut cursor = 0usize;
    while cursor < arguments.len() {
        let (name, inline) = split_flag(&arguments[cursor]);
        match name {
            "--apply" => flags.apply = bool_value(inline, "--apply")?,
            "--max-age" => flags.max_age = int_value(arguments, &mut cursor, "--max-age", inline)?,
            "--min-score" => {
                flags.min_score = float_value(arguments, &mut cursor, "--min-score", inline)?;
            }
            "--max-count" => {
                flags.max_count = int_value(arguments, &mut cursor, "--max-count", inline)?;
            }
            other if other.starts_with('-') && other != "-" => {
                return Err(CmdError::UnknownFlag(other.to_owned()));
            }
            _ => {}
        }
        cursor += 1;
    }
    Ok(flags)
}

fn parse_archive(arguments: &[String]) -> Result<ArchiveFlags, CmdError> {
    let mut flags = ArchiveFlags::default();
    let mut cursor = 0usize;
    while cursor < arguments.len() {
        let (name, inline) = split_flag(&arguments[cursor]);
        match name {
            "--apply" => flags.apply = bool_value(inline, "--apply")?,
            "--domain" => flags.domain = string_value(arguments, &mut cursor, "--domain", inline)?,
            "--db" => flags.db = string_value(arguments, &mut cursor, "--db", inline)?,
            other if other.starts_with('-') && other != "-" => {
                return Err(CmdError::UnknownFlag(other.to_owned()));
            }
            _ => {}
        }
        cursor += 1;
    }
    Ok(flags)
}

fn parse_backfill(arguments: &[String]) -> Result<BackfillFlags, CmdError> {
    let mut flags = BackfillFlags::default();
    let mut cursor = 0usize;
    while cursor < arguments.len() {
        let (name, inline) = split_flag(&arguments[cursor]);
        match name {
            "--apply" => flags.apply = bool_value(inline, "--apply")?,
            "--since" => {
                let literal = string_value(arguments, &mut cursor, "--since", inline)?;
                flags.since = parse_go_duration(&literal).ok_or(CmdError::InvalidFlagValue {
                    flag: "--since".to_owned(),
                })?;
            }
            "--limit" => flags.limit = int_value(arguments, &mut cursor, "--limit", inline)?,
            "--batch-size" => {
                flags.batch_size = int_value(arguments, &mut cursor, "--batch-size", inline)?;
            }
            "--rpm" => flags.rpm = int_value(arguments, &mut cursor, "--rpm", inline)?,
            "--max-retries" => {
                flags.max_retries = int_value(arguments, &mut cursor, "--max-retries", inline)?;
            }
            "--db" => flags.db = string_value(arguments, &mut cursor, "--db", inline)?,
            "--include-archived" => {
                flags.include_archived = bool_value(inline, "--include-archived")?;
            }
            "--quiet" => flags.quiet = bool_value(inline, "--quiet")?,
            other if other.starts_with('-') && other != "-" => {
                return Err(CmdError::UnknownFlag(other.to_owned()));
            }
            _ => {}
        }
        cursor += 1;
    }
    Ok(flags)
}

/// pflag boolean flags take no separate value token; only the inline
/// `--flag=value` form carries one.
fn bool_value(inline: Option<&str>, flag: &str) -> Result<bool, CmdError> {
    match inline {
        None => Ok(true),
        Some(value) => parse_bool_flag_value(value, flag),
    }
}

fn string_value(
    arguments: &[String],
    cursor: &mut usize,
    flag: &str,
    inline: Option<&str>,
) -> Result<String, CmdError> {
    match inline {
        Some(value) => Ok(value.to_owned()),
        None => next_string_value(arguments, cursor, flag),
    }
}

fn int_value(
    arguments: &[String],
    cursor: &mut usize,
    flag: &str,
    inline: Option<&str>,
) -> Result<i64, CmdError> {
    let literal = string_value(arguments, cursor, flag, inline)?;
    parse_go_int64(&literal).ok_or_else(|| CmdError::InvalidFlagValue {
        flag: flag.to_owned(),
    })
}

fn float_value(
    arguments: &[String],
    cursor: &mut usize,
    flag: &str,
    inline: Option<&str>,
) -> Result<f64, CmdError> {
    let literal = string_value(arguments, cursor, flag, inline)?;
    literal
        .parse::<f64>()
        .map_err(|_| CmdError::InvalidFlagValue {
            flag: flag.to_owned(),
        })
}

/// Ports `time.ParseDuration`: a possibly-signed sequence of decimal numbers
/// each with an optional fraction and a required unit, e.g. `300ms`,
/// `-1.5h`, `2h45m`. A bare `0` is accepted with no unit; nothing else is.
///
/// Overflow returns `None`, matching Go's `invalid duration` error rather than
/// wrapping.
fn parse_go_duration(text: &str) -> Option<GoDuration> {
    let original = text;
    let mut rest = text;
    let mut negative = false;
    match rest.as_bytes().first() {
        Some(b'-') => {
            negative = true;
            rest = &rest[1..];
        }
        Some(b'+') => rest = &rest[1..],
        _ => {}
    }
    if rest == "0" {
        return Some(GoDuration {
            nanos: 0,
            literal: original.to_owned(),
        });
    }
    if rest.is_empty() {
        return None;
    }

    let mut total: i64 = 0;
    while !rest.is_empty() {
        // Mantissa: digits, optional '.', digits. At least one digit overall.
        let integer_len = rest.bytes().take_while(u8::is_ascii_digit).count();
        let mut cursor = integer_len;
        let mut fraction = "";
        if rest.as_bytes().get(cursor) == Some(&b'.') {
            cursor += 1;
            let fraction_len = rest[cursor..]
                .bytes()
                .take_while(u8::is_ascii_digit)
                .count();
            fraction = &rest[cursor..cursor + fraction_len];
            cursor += fraction_len;
        }
        if integer_len == 0 && fraction.is_empty() {
            return None;
        }
        let integer: i64 = if integer_len == 0 {
            0
        } else {
            rest[..integer_len].parse().ok()?
        };

        let unit_len = rest[cursor..]
            .bytes()
            .take_while(|byte| !byte.is_ascii_digit() && *byte != b'.')
            .count();
        let unit = &rest[cursor..cursor + unit_len];
        let scale: i64 = match unit {
            "ns" => 1,
            "us" | "\u{b5}s" | "\u{3bc}s" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60 * 1_000_000_000,
            "h" => 3_600 * 1_000_000_000,
            _ => return None,
        };

        let mut nanos = integer.checked_mul(scale)?;
        if !fraction.is_empty() {
            // Go accumulates the fraction as scale * 0.<digits>, truncating.
            let mut divisor = 1f64;
            let mut value = 0f64;
            for digit in fraction.bytes() {
                divisor *= 10.0;
                value += f64::from(digit - b'0') / divisor;
            }
            #[allow(clippy::cast_possible_truncation)]
            let fractional = (value * scale as f64) as i64;
            nanos = nanos.checked_add(fractional)?;
        }
        total = total.checked_add(nanos)?;
        rest = &rest[cursor + unit_len..];
    }

    Some(GoDuration {
        nanos: if negative {
            total.checked_neg()?
        } else {
            total
        },
        literal: original.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Runs one learning-maintenance command against an injected read-only store.
///
/// The `--apply` check happens first and without touching `reader`, so a
/// refused write never even opens the store. Callers can rely on that ordering:
/// the gate proves it by pointing an `--apply` invocation at a fixture with no
/// `learnings.db` and asserting the refusal is still
/// [`LearningError::ApplyNotPorted`] rather than a read failure.
pub(crate) fn run<R: GoLearningReader + ?Sized>(
    reader: &R,
    command: &LearningCommand,
    output: &mut impl Write,
) -> Result<(), LearningError> {
    if let Some(error) = apply_refusal(command) {
        return Err(error);
    }
    match command {
        LearningCommand::Stats => run_stats(reader, output),
        LearningCommand::Prune(flags) => run_prune(reader, flags, output),
        LearningCommand::Archive(flags) => run_archive(reader, flags, output),
        LearningCommand::BackfillEmbeddings(flags) => run_backfill(reader, flags, output),
    }
}

/// The refusal for each command's `--apply`, naming the Go writes it reaches.
fn apply_refusal(command: &LearningCommand) -> Option<LearningError> {
    if !command.is_apply() {
        return None;
    }
    let operations = match command {
        LearningCommand::Stats => return None,
        LearningCommand::Prune(_) => {
            "learning.DB.Cleanup deletes rows and decayScores updates them"
        }
        LearningCommand::Archive(_) => {
            "learning.DB.UpdateQualityScores and ArchiveDeadWeight update rows"
        }
        LearningCommand::BackfillEmbeddings(_) => "learning.DB.SetEmbedding updates rows",
    };
    Some(LearningError::ApplyNotPorted {
        command: command.name(),
        operations,
    })
}

/// Seconds since the Unix epoch, or `0` if the clock is before it.
fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0))
}

/// Reads every non-archived-filtered row Go's predicates would scan.
///
/// Go runs its counts as SQL over the whole table; this reads one bounded page
/// and refuses a truncated one rather than undercounting.
fn read_all_rows<R: GoLearningReader + ?Sized>(
    reader: &R,
    command: &'static str,
    domain: Option<String>,
    include_archived: bool,
) -> Result<Vec<LearningRow>, LearningError> {
    let page = reader
        .list(&LearningListQuery {
            domain,
            learning_type: None,
            include_archived,
            limit: MAX_ADAPTER_ROWS,
        })
        .map_err(LearningError::Read)?;
    if page.truncated {
        return Err(LearningError::StoreTooLarge {
            command,
            limit: MAX_ADAPTER_ROWS,
        });
    }
    Ok(page.rows)
}

/// Ports `showStats` (`learn.go:66-90`).
///
/// Both lines come from SQL aggregates on [`LearningStats`], so this path is
/// exact for any store size — unlike the row-counting commands below.
fn run_stats<R: GoLearningReader + ?Sized>(
    reader: &R,
    output: &mut impl Write,
) -> Result<(), LearningError> {
    let stats = reader.stats().map_err(LearningError::Read)?;
    writeln!(
        output,
        "learnings: {} total, {} with embeddings",
        stats.total, stats.with_embeddings
    )?;
    if stats.injected > 0 {
        // Go prints `%.0f%%` of `avgRate*100`; Rust's `{:.0}` rounds
        // half-to-even, as Go's `strconv` formatting does.
        writeln!(
            output,
            "compliance:  {} injected, avg rate {:.0}%",
            stats.injected,
            stats.avg_compliance_rate * 100.0
        )?;
    } else {
        writeln!(output, "compliance:  no injections recorded yet")?;
    }
    Ok(())
}

/// Ports `runPrune` (`learn.go:92-122`) over `learning.DB.Cleanup`'s three
/// dry-run criteria (`db.go:955-1033`), which Go sums into one number:
///
/// 1. `created_at < now-max_age AND quality_score < 0.5 AND used_count = 0`
/// 2. `quality_score < --min-score`
/// 3. per-domain excess over `--max-count`
///
/// Rows are counted, not deleted: criteria 1 and 2 overlap in Go too, and Go
/// double-counts the overlap in its dry-run sum, so this must as well.
fn run_prune<R: GoLearningReader + ?Sized>(
    reader: &R,
    flags: &PruneFlags,
    output: &mut impl Write,
) -> Result<(), LearningError> {
    // Go's Cleanup normalizes non-positive values to its defaults before
    // querying (`db.go:955-963`), so a `--max-age 0` is not "no age filter".
    let max_age_days = if flags.max_age <= 0 {
        180
    } else {
        flags.max_age
    };
    let min_score = if flags.min_score <= 0.0 {
        0.1
    } else {
        flags.min_score
    };
    let max_per_domain = if flags.max_count <= 0 {
        500
    } else {
        flags.max_count
    };

    // Go compares `created_at` against a `time.RFC3339` string, and SQLite
    // compares TEXT lexicographically, so the cutoff is built the same way.
    let cutoff = format_unix_seconds_rfc3339(
        now_unix_seconds().saturating_sub(max_age_days.saturating_mul(86_400)),
    );

    // Cleanup queries the whole table; none of its three statements filters on
    // `archived`.
    let rows = read_all_rows(reader, "prune", None, true)?;

    let mut total_removed: i64 = 0;
    for row in &rows {
        if row.created_at.as_str() < cutoff.as_str()
            && row.quality_score < 0.5
            && row.used_count == 0
        {
            total_removed += 1;
        }
        if row.quality_score < min_score {
            total_removed += 1;
        }
    }

    // `GROUP BY domain HAVING cnt > ?`, then `excess = cnt - max`.
    let mut domains: Vec<(&str, i64)> = Vec::new();
    for row in &rows {
        match domains.iter_mut().find(|(name, _)| *name == row.domain) {
            Some((_, count)) => *count += 1,
            None => domains.push((row.domain.as_str(), 1)),
        }
    }
    for (_, count) in &domains {
        let excess = count - max_per_domain;
        if excess > 0 {
            total_removed += excess;
        }
    }

    writeln!(
        output,
        "dry-run: would remove {total_removed} learnings (use --apply to delete)"
    )?;
    Ok(())
}

/// One `ArchiveDeadWeight` criterion: Go's reason string and its predicate.
struct ArchiveCriterion {
    reason: &'static str,
    /// `(row, cutoffs)` -> whether the row matches. Cutoffs are the SQLite
    /// `datetime('now', '-N days')` strings for 90/60/30 days.
    matches: fn(&LearningRow, &ArchiveCutoffs) -> bool,
}

/// The three `datetime('now', ...)` boundaries Go's criteria compare against.
struct ArchiveCutoffs {
    ninety_days: String,
    sixty_days: String,
    thirty_days: String,
}

/// Go's five criteria in registration order (`db.go:1205-1240`). The order is
/// load-bearing: the first criterion to claim an id owns the printed reason.
const ARCHIVE_CRITERIA: &[ArchiveCriterion] = &[
    ArchiveCriterion {
        reason: "never injected, older than 90 days",
        matches: |row, cutoffs| {
            row.injection_count == 0
                && row.used_count == 0
                && row.created_at.as_str() < cutoffs.ninety_days.as_str()
        },
    },
    ArchiveCriterion {
        reason: "chronic non-compliance (injection_count >= 5, compliance_rate < 0.10)",
        matches: |row, _| row.injection_count >= 5 && row.compliance_rate < 0.10,
    },
    ArchiveCriterion {
        reason: "low quality, never used, older than 60 days",
        matches: |row, cutoffs| {
            row.quality_score < 0.2
                && row.used_count == 0
                && row.created_at.as_str() < cutoffs.sixty_days.as_str()
        },
    },
    ArchiveCriterion {
        reason: "single observation, no embedding, older than 30 days",
        matches: |row, cutoffs| {
            row.seen_count == 1
                && !row.has_embedding
                && row.created_at.as_str() < cutoffs.thirty_days.as_str()
        },
    },
    ArchiveCriterion {
        reason: "stale_injected",
        matches: |row, _| row.injection_count > 100 && row.compliance_rate < 0.25,
    },
];

/// Ports `runArchive` (`archive.go:35-83`) over `ArchiveDeadWeight`'s five
/// criteria (`db.go:1192-1245`).
///
/// Every criterion carries `archived = 0`, which is expressed here by asking
/// the reader not to include archived rows, and the optional `AND domain = ?`,
/// which is the reader's domain filter.
fn run_archive<R: GoLearningReader + ?Sized>(
    reader: &R,
    flags: &ArchiveFlags,
    output: &mut impl Write,
) -> Result<(), LearningError> {
    let mut rows = read_all_rows(
        reader,
        "archive",
        (!flags.domain.is_empty()).then(|| flags.domain.clone()),
        false,
    )?;
    // See the module doc: Go scans in `rowid` order, which for a Go-written
    // store is creation order. `list` returns `created_at DESC`, so re-sort.
    rows.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });

    let now = now_unix_seconds();
    let cutoffs = ArchiveCutoffs {
        ninety_days: format_unix_seconds_ymd_hms(now - 90 * 86_400),
        sixty_days: format_unix_seconds_ymd_hms(now - 60 * 86_400),
        thirty_days: format_unix_seconds_ymd_hms(now - 30 * 86_400),
    };

    // `--apply` is refused upstream, so the recompute is always the dry-run
    // line (`archive.go:52-56`).
    writeln!(output, "dry-run: would recompute quality scores")?;

    let mut candidates: Vec<(&str, &'static str)> = Vec::new();
    for criterion in ARCHIVE_CRITERIA {
        for row in &rows {
            if (criterion.matches)(row, &cutoffs) && !candidates.iter().any(|(id, _)| *id == row.id)
            {
                candidates.push((row.id.as_str(), criterion.reason));
            }
        }
    }

    if candidates.is_empty() {
        writeln!(output, "no dead-weight learnings found")?;
        return Ok(());
    }
    for (id, reason) in &candidates {
        writeln!(output, "would archive {id}: {reason}")?;
    }
    writeln!(
        output,
        "\ndry-run: {} learnings would be archived (use --apply to write)",
        candidates.len()
    )?;
    Ok(())
}

/// Ports `runBackfillEmbeddings` (`learn.go:124-224`) up to its dry-run
/// return. Go's two exit-2 validations run before the store is opened, so they
/// are checked here in the same order.
///
/// The candidate scan is `CountEmbeddingBackfill` (`db.go:874-892`):
/// `embedding IS NULL`, `archived = 0` unless `--include-archived`, and
/// `created_at >= now-since` when `--since` is non-zero.
fn run_backfill<R: GoLearningReader + ?Sized>(
    reader: &R,
    flags: &BackfillFlags,
    output: &mut impl Write,
) -> Result<(), LearningError> {
    if flags.batch_size <= 0 || flags.batch_size > 100 {
        return Err(LearningError::InvalidBatchSize {
            value: flags.batch_size,
        });
    }
    if flags.since.nanos() < 0 {
        return Err(LearningError::InvalidSince {
            value: flags.since.literal.clone(),
        });
    }

    let rows = read_all_rows(reader, "backfill-embeddings", None, flags.include_archived)?;
    let since_cutoff = (flags.since.nanos() > 0).then(|| {
        format_unix_seconds_rfc3339(
            now_unix_seconds().saturating_sub(flags.since.nanos() / 1_000_000_000),
        )
    });

    let mut store_rows: i64 = 0;
    let mut total_chars: i64 = 0;
    for row in &rows {
        if !row.has_embedding
            && since_cutoff
                .as_ref()
                .is_none_or(|cutoff| row.created_at.as_str() >= cutoff.as_str())
        {
            store_rows += 1;
            // SQLite's `LENGTH()` on TEXT counts characters, not bytes.
            total_chars += i64::try_from(row.content.chars().count()).unwrap_or(i64::MAX);
        }
    }

    let mut candidate_rows = store_rows;
    if flags.limit > 0 && flags.limit < candidate_rows {
        candidate_rows = flags.limit;
    }
    #[allow(clippy::cast_possible_truncation)]
    let mut est_tokens = ((total_chars as f64) / EMBEDDING_CHARS_PER_TOKEN_EST) as i64;
    if flags.limit > 0 && store_rows > 0 && flags.limit < store_rows {
        // Go scales the token estimate to the capped row count, assuming
        // uniform content length (`learn.go:166-169`).
        est_tokens = est_tokens.saturating_mul(flags.limit) / store_rows;
    }
    #[allow(clippy::cast_precision_loss)]
    let est_usd = (est_tokens as f64) * EMBEDDING_PRICE_PER_M_TOKENS / 1_000_000.0;
    let batches = (candidate_rows + flags.batch_size - 1) / flags.batch_size;
    let wall_seconds = if flags.rpm > 0 {
        (batches * 60) / flags.rpm
    } else {
        0
    };

    writeln!(output, "embed-backfill (dry-run):")?;
    writeln!(output, "  candidate rows: {candidate_rows}")?;
    writeln!(output, "  est. tokens:    {est_tokens} (chars/4 heuristic)")?;
    writeln!(
        output,
        "  est. cost:      ${est_usd:.4} USD @ ${EMBEDDING_PRICE_PER_M_TOKENS:.2} per 1M tokens"
    )?;
    writeln!(
        output,
        "  batches:        {batches} \u{d7} {} (estimated wall time: ~{wall_seconds}s at {} rpm)",
        flags.batch_size, flags.rpm
    )?;
    writeln!(output, "  run with --apply to write embeddings")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ArchiveFlags, BackfillFlags, LearningCommand, LearningError, PruneFlags, parse,
        parse_go_duration, run,
    };
    use orchestrator_app::{
        GoAdapterError, GoLearningReader, LearningListQuery, LearningPage, LearningRow,
        LearningStats, RowSetDigest, TopQualityQuery,
    };
    use std::cell::RefCell;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    /// Records whether the store was touched at all, so the ordering claim in
    /// [`run`]'s doc comment is checked rather than asserted.
    #[derive(Default)]
    struct RecordingReader {
        reads: RefCell<usize>,
    }

    impl RecordingReader {
        fn reads(&self) -> usize {
            *self.reads.borrow()
        }
    }

    impl GoLearningReader for RecordingReader {
        fn schema_version(&self) -> Result<u32, GoAdapterError> {
            *self.reads.borrow_mut() += 1;
            Ok(1)
        }
        fn list(&self, _query: &LearningListQuery) -> Result<LearningPage, GoAdapterError> {
            *self.reads.borrow_mut() += 1;
            Ok(LearningPage {
                rows: Vec::new(),
                truncated: false,
            })
        }
        fn top_by_quality(
            &self,
            _query: &TopQualityQuery,
        ) -> Result<Vec<LearningRow>, GoAdapterError> {
            *self.reads.borrow_mut() += 1;
            Ok(Vec::new())
        }
        fn stats(&self) -> Result<LearningStats, GoAdapterError> {
            *self.reads.borrow_mut() += 1;
            Ok(LearningStats::default())
        }
        fn row_set_digest(&self) -> Result<RowSetDigest, GoAdapterError> {
            *self.reads.borrow_mut() += 1;
            Err(GoAdapterError::Read {
                operation: "unused in this fixture",
            })
        }
        fn digest_components(&self) -> Result<Vec<(String, String)>, GoAdapterError> {
            *self.reads.borrow_mut() += 1;
            Ok(Vec::new())
        }
    }

    #[test]
    fn every_command_defaults_to_dry_run() -> TestResult {
        for arguments in [
            vec!["stats".to_owned()],
            vec!["prune".to_owned()],
            vec!["archive".to_owned()],
            vec!["backfill-embeddings".to_owned()],
        ] {
            let (name, rest) = arguments.split_first().ok_or("empty case")?;
            let command = parse(name, rest)?;
            assert!(!command.is_apply(), "{name} defaulted to apply");
        }
        Ok(())
    }

    #[test]
    fn apply_is_refused_before_the_store_is_touched() -> TestResult {
        for (name, rest) in [
            ("prune", vec!["--apply".to_owned()]),
            ("archive", vec!["--apply".to_owned()]),
            ("backfill-embeddings", vec!["--apply".to_owned()]),
        ] {
            let command = parse(name, &rest)?;
            assert!(command.is_apply());
            let reader = RecordingReader::default();
            let mut output = Vec::new();
            let result = run(&reader, &command, &mut output);
            assert!(
                matches!(result, Err(LearningError::ApplyNotPorted { .. })),
                "{name} did not refuse --apply"
            );
            assert_eq!(reader.reads(), 0, "{name} read the store despite refusing");
            assert!(output.is_empty(), "{name} wrote output despite refusing");
        }
        Ok(())
    }

    #[test]
    fn dry_run_reads_the_store_then_renders_its_data_lines() -> TestResult {
        for (name, rest) in [
            ("stats", Vec::new()),
            ("prune", Vec::new()),
            ("archive", Vec::new()),
            ("backfill-embeddings", Vec::new()),
        ] {
            let command = parse(name, &rest)?;
            let reader = RecordingReader::default();
            let mut output = Vec::new();
            run(&reader, &command, &mut output)
                .map_err(|error| format!("{name} failed: {error}"))?;
            assert_eq!(reader.reads(), 1, "{name} did not read the store first");
            assert!(!output.is_empty(), "{name} rendered nothing");
        }
        Ok(())
    }

    #[test]
    fn an_empty_store_renders_gos_empty_wordings() -> TestResult {
        // The empty-store branches of `showStats` and `runArchive` are the two
        // lines the oracle fixture (which is never empty) cannot reach.
        let reader = RecordingReader::default();
        let mut output = Vec::new();
        run(&reader, &parse("stats", &[])?, &mut output)?;
        assert_eq!(
            String::from_utf8(output)?,
            "learnings: 0 total, 0 with embeddings\ncompliance:  no injections recorded yet\n"
        );

        let mut output = Vec::new();
        run(&reader, &parse("archive", &[])?, &mut output)?;
        assert_eq!(
            String::from_utf8(output)?,
            "dry-run: would recompute quality scores\nno dead-weight learnings found\n"
        );
        Ok(())
    }

    #[test]
    fn a_truncated_page_fails_closed_rather_than_undercounting() -> TestResult {
        // `list` bounds at MAX_ADAPTER_ROWS; a store past that bound would make
        // the row-counting commands print a number that is quietly too small.
        struct TruncatingReader;
        impl GoLearningReader for TruncatingReader {
            fn schema_version(&self) -> Result<u32, GoAdapterError> {
                Ok(1)
            }
            fn list(&self, _query: &LearningListQuery) -> Result<LearningPage, GoAdapterError> {
                Ok(LearningPage {
                    rows: Vec::new(),
                    truncated: true,
                })
            }
            fn top_by_quality(
                &self,
                _query: &TopQualityQuery,
            ) -> Result<Vec<LearningRow>, GoAdapterError> {
                Ok(Vec::new())
            }
            fn stats(&self) -> Result<LearningStats, GoAdapterError> {
                Ok(LearningStats::default())
            }
            fn row_set_digest(&self) -> Result<RowSetDigest, GoAdapterError> {
                Err(GoAdapterError::Read {
                    operation: "unused in this fixture",
                })
            }
            fn digest_components(&self) -> Result<Vec<(String, String)>, GoAdapterError> {
                Ok(Vec::new())
            }
        }

        for name in ["prune", "archive", "backfill-embeddings"] {
            let command = parse(name, &[])?;
            let mut output = Vec::new();
            let result = run(&TruncatingReader, &command, &mut output);
            assert!(
                matches!(result, Err(LearningError::StoreTooLarge { .. })),
                "{name} counted a truncated page"
            );
            assert!(output.is_empty(), "{name} wrote a partial line");
        }
        Ok(())
    }

    #[test]
    fn backfill_validations_run_before_the_store_is_opened() -> TestResult {
        for (rest, expected_batch) in [
            (vec!["--batch-size=0".to_owned()], true),
            (vec!["--batch-size=101".to_owned()], true),
        ] {
            let command = parse("backfill-embeddings", &rest)?;
            let reader = RecordingReader::default();
            let mut output = Vec::new();
            let result = run(&reader, &command, &mut output);
            assert_eq!(
                matches!(result, Err(LearningError::InvalidBatchSize { .. })),
                expected_batch
            );
            assert_eq!(reader.reads(), 0);
        }

        let command = parse("backfill-embeddings", &["--since=-5s".to_owned()])?;
        let reader = RecordingReader::default();
        let mut output = Vec::new();
        assert!(matches!(
            run(&reader, &command, &mut output),
            Err(LearningError::InvalidSince { .. })
        ));
        assert_eq!(reader.reads(), 0);
        Ok(())
    }

    #[test]
    fn flag_defaults_match_the_go_registrations() -> TestResult {
        assert_eq!(
            parse("prune", &[])?,
            LearningCommand::Prune(PruneFlags {
                apply: false,
                max_age: 180,
                min_score: 0.1,
                max_count: 500,
            })
        );
        assert_eq!(
            parse("archive", &[])?,
            LearningCommand::Archive(ArchiveFlags::default())
        );
        assert_eq!(
            parse("backfill-embeddings", &[])?,
            LearningCommand::BackfillEmbeddings(BackfillFlags::default())
        );
        Ok(())
    }

    #[test]
    fn flags_accept_both_pflag_value_forms() -> TestResult {
        let inline = parse(
            "prune",
            &["--max-age=7".to_owned(), "--apply=true".to_owned()],
        )?;
        let separate = parse(
            "prune",
            &["--max-age".to_owned(), "7".to_owned(), "--apply".to_owned()],
        )?;
        assert_eq!(inline, separate);
        Ok(())
    }

    #[test]
    fn every_flag_changes_the_field_it_names() -> TestResult {
        // Acceptance is not enough: a flag that parses but is discarded would
        // pass an "is it rejected?" check while silently ignoring the operator.
        // Each value below differs from the command's default.
        let prune_default = parse("prune", &[])?;
        for argument in ["--apply", "--max-age=1", "--min-score=0.9", "--max-count=1"] {
            let parsed = parse("prune", &[argument.to_owned()])?;
            assert_ne!(parsed, prune_default, "prune ignored {argument}");
        }

        let archive_default = parse("archive", &[])?;
        for argument in ["--apply", "--domain=work", "--db=/dev/null"] {
            let parsed = parse("archive", &[argument.to_owned()])?;
            assert_ne!(parsed, archive_default, "archive ignored {argument}");
        }

        let backfill_default = parse("backfill-embeddings", &[])?;
        for argument in [
            "--apply",
            "--since=1h",
            "--limit=1",
            "--batch-size=1",
            "--rpm=1",
            "--max-retries=1",
            "--db=/dev/null",
            "--include-archived",
            "--quiet",
        ] {
            let parsed = parse("backfill-embeddings", &[argument.to_owned()])?;
            assert_ne!(
                parsed, backfill_default,
                "backfill-embeddings ignored {argument}"
            );
        }
        Ok(())
    }

    #[test]
    fn go_duration_grammar_round_trips() {
        for (text, nanos) in [
            ("0", 0i64),
            ("720h", 720 * 3_600 * 1_000_000_000),
            ("1h30m", 90 * 60 * 1_000_000_000),
            ("300ms", 300_000_000),
            ("1.5s", 1_500_000_000),
            ("-5s", -5_000_000_000),
        ] {
            let parsed = parse_go_duration(text);
            assert!(parsed.is_some(), "rejected {text}");
            assert_eq!(parsed.map(|value| value.nanos()), Some(nanos), "{text}");
        }
        for text in ["", "5", "abc", "1x", "-"] {
            assert!(parse_go_duration(text).is_none(), "accepted {text}");
        }
    }

    #[test]
    fn stats_rejects_flags_like_cobra_does() {
        assert!(parse("stats", &["--apply".to_owned()]).is_err());
    }
}
