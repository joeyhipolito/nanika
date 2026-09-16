//! Gate 5 — `cargo test -p orchestrator-app --locked --test sqlite_compat_matrix`.
//!
//! Proves the adapters read every schema shape a real installation can present.
//!
//! The Go learning store reaches its current shape two different ways. A
//! database created by a current binary gets the whole `learnings` table from
//! one `CREATE TABLE`. A database created by an older binary gets a narrower
//! table plus the `ALTER TABLE` list at `internal/learning/db.go:165-176`,
//! which `db.go` applies with `d.db.Exec(m)` and *ignores the errors from*. The
//! two produce different column orders in `sqlite_schema`, and Go reads both
//! identically — so the Rust adapter must too.
//!
//! The matrix therefore covers, for every supported `schema_version`:
//!
//! - fresh `CREATE TABLE` form vs. old-table-plus-migrations form;
//! - WAL and rollback-journal modes;
//! - a `NULL` `embedding` row;
//! - a version above the supported maximum, which must yield `SchemaTooNew`
//!   rather than a partial read (`db.go:105-107`);
//! - the FTS5 external-content table and its three triggers surviving an
//!   additive write intact;
//! - `metrics.db` both before and after the `quota_snapshots` /
//!   `usage_snapshots` / `idx_phases_worker_name` migrations.
//!
//! Every schema variant is generated at test time by executing DDL text quoted
//! from the Go source into a fresh temp database, so the matrix stays honest if
//! the Go schema changes rather than drifting against a checked-in fixture.

#![allow(clippy::doc_markdown, reason = "prose names Go symbols and file paths")]

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use orchestrator_app::{
    AdditiveFixtureGrant, GoAdapterError, GoLearningAdapter, GoLearningAppender, GoLearningReader,
    IsolatedFixtureRoot, LearningListQuery, MAX_SUPPORTED_LEARNING_SCHEMA_VERSION, MetricsOwner,
    MetricsOwnerCapability, NewLearningRow, PhaseMetricIntent, TopQualityQuery,
};
use orchestrator_core::MissionId;
use rusqlite::Connection;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static CASE: AtomicU64 = AtomicU64::new(1);

// ===========================================================================
// Go DDL, quoted from source
// ===========================================================================

/// `schema_version` bootstrap (`internal/learning/db.go:86-97`).
const SCHEMA_VERSION_DDL: &str = "CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER NOT NULL
);";

/// The current `learnings` table (`internal/learning/db.go:107-123`).
///
/// A binary at this version creates every column at once.
const LEARNINGS_FRESH_DDL: &str = "CREATE TABLE IF NOT EXISTS learnings (
    id TEXT PRIMARY KEY,
    type TEXT NOT NULL,
    content TEXT NOT NULL,
    context TEXT DEFAULT '',
    domain TEXT NOT NULL,
    worker_name TEXT DEFAULT '',
    workspace_id TEXT DEFAULT '',
    tags TEXT DEFAULT '',
    seen_count INTEGER DEFAULT 1,
    used_count INTEGER DEFAULT 0,
    quality_score REAL DEFAULT 0.0,
    created_at DATETIME NOT NULL,
    last_used_at DATETIME
);";

/// The `learnings` table as an *older* binary left it — before the migration
/// list added `worker_name`, `workspace_id`, `archived`, and the rest.
const LEARNINGS_LEGACY_DDL: &str = "CREATE TABLE IF NOT EXISTS learnings (
    id TEXT PRIMARY KEY,
    type TEXT NOT NULL,
    content TEXT NOT NULL,
    context TEXT DEFAULT '',
    domain TEXT NOT NULL,
    tags TEXT DEFAULT '',
    seen_count INTEGER DEFAULT 1,
    used_count INTEGER DEFAULT 0,
    quality_score REAL DEFAULT 0.0,
    created_at DATETIME NOT NULL,
    last_used_at DATETIME
);";

/// The additive migrations (`internal/learning/db.go:165-176`).
///
/// Go runs these unconditionally and discards the error, because on a current
/// database each one fails with "duplicate column name". Both outcomes must
/// leave a database the adapter reads identically.
const LEARNINGS_MIGRATIONS: &[&str] = &[
    "ALTER TABLE learnings ADD COLUMN worker_name TEXT DEFAULT ''",
    "ALTER TABLE learnings ADD COLUMN workspace_id TEXT DEFAULT ''",
    "ALTER TABLE learnings ADD COLUMN injection_count INTEGER DEFAULT 0",
    "ALTER TABLE learnings ADD COLUMN compliance_count INTEGER DEFAULT 0",
    "ALTER TABLE learnings ADD COLUMN compliance_rate REAL DEFAULT 0.0",
    "ALTER TABLE learnings ADD COLUMN archived INTEGER DEFAULT 0",
    "ALTER TABLE learnings ADD COLUMN promoted_at DATETIME",
];

/// The `embedding` column. On a fresh database it is part of the `CREATE
/// TABLE`; here it is applied separately so both forms can reach it.
const EMBEDDING_MIGRATION: &str = "ALTER TABLE learnings ADD COLUMN embedding BLOB";

/// The FTS5 external-content table and its triggers
/// (`internal/learning/db.go:129-160`).
const LEARNINGS_FTS_DDL: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS learnings_fts USING fts5(
    content, context, domain,
    content='learnings', content_rowid='rowid'
);
CREATE TRIGGER IF NOT EXISTS learnings_ai AFTER INSERT ON learnings BEGIN
    INSERT INTO learnings_fts(rowid, content, context, domain)
    VALUES (new.rowid, new.content, new.context, new.domain);
END;
CREATE TRIGGER IF NOT EXISTS learnings_ad AFTER DELETE ON learnings BEGIN
    INSERT INTO learnings_fts(learnings_fts, rowid, content, context, domain)
    VALUES ('delete', old.rowid, old.content, old.context, old.domain);
END;
CREATE TRIGGER IF NOT EXISTS learnings_au AFTER UPDATE ON learnings BEGIN
    INSERT INTO learnings_fts(learnings_fts, rowid, content, context, domain)
    VALUES ('delete', old.rowid, old.content, old.context, old.domain);
    INSERT INTO learnings_fts(rowid, content, context, domain)
    VALUES (new.rowid, new.content, new.context, new.domain);
END;";

const LEARNINGS_INDEXES: &str = "CREATE INDEX IF NOT EXISTS idx_learnings_domain
    ON learnings(domain);
CREATE INDEX IF NOT EXISTS idx_learnings_type ON learnings(type);
CREATE INDEX IF NOT EXISTS idx_learnings_domain_archived
    ON learnings(domain, archived);";

/// A `metrics.db` as an older Go binary left it: the three core tables, but
/// none of the snapshot tables or later columns.
///
/// Quoted from the pre-migration shape implied by
/// `internal/metrics/db.go:352-405` — every `ALTER TABLE` in that list names a
/// column that a database of this vintage does not have.
const METRICS_LEGACY_DDL: &str = "CREATE TABLE missions (
    id                  TEXT PRIMARY KEY,
    domain              TEXT NOT NULL DEFAULT '',
    task                TEXT NOT NULL DEFAULT '',
    started_at          DATETIME NOT NULL,
    finished_at         DATETIME NOT NULL,
    duration_s          INTEGER NOT NULL DEFAULT 0,
    phases_total        INTEGER NOT NULL DEFAULT 0,
    phases_completed    INTEGER NOT NULL DEFAULT 0,
    phases_failed       INTEGER NOT NULL DEFAULT 0,
    phases_skipped      INTEGER NOT NULL DEFAULT 0,
    learnings_retrieved INTEGER NOT NULL DEFAULT 0,
    retries_total       INTEGER NOT NULL DEFAULT 0,
    gate_failures       INTEGER NOT NULL DEFAULT 0,
    output_len_total    INTEGER NOT NULL DEFAULT 0,
    status              TEXT NOT NULL DEFAULT ''
);
CREATE TABLE phases (
    id                  TEXT PRIMARY KEY,
    mission_id          TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
    name                TEXT NOT NULL DEFAULT '',
    persona             TEXT NOT NULL DEFAULT '',
    duration_s          INTEGER NOT NULL DEFAULT 0,
    status              TEXT NOT NULL DEFAULT '',
    retries             INTEGER NOT NULL DEFAULT 0,
    gate_passed         INTEGER NOT NULL DEFAULT 0,
    output_len          INTEGER NOT NULL DEFAULT 0,
    learnings_retrieved INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE skill_invocations (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
    persona    TEXT NOT NULL DEFAULT '',
    skill_name TEXT NOT NULL DEFAULT '',
    invoked_at DATETIME NOT NULL
);";

// ===========================================================================
// Matrix axes
// ===========================================================================

/// Which way the database reached its current shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Shape {
    /// One `CREATE TABLE` with every column.
    Fresh,
    /// A narrower `CREATE TABLE` plus the `ALTER TABLE` migration list.
    Migrated,
}

impl Shape {
    const ALL: [Self; 2] = [Self::Fresh, Self::Migrated];

    const fn label(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Migrated => "migrated",
        }
    }
}

/// SQLite journal mode. Both are real deployment states: Go sets WAL on open,
/// but a database copied while closed, or one on a filesystem that refuses
/// shared memory, presents the rollback journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Journal {
    Wal,
    Delete,
}

impl Journal {
    const ALL: [Self; 2] = [Self::Wal, Self::Delete];

    const fn pragma(self) -> &'static str {
        match self {
            Self::Wal => "PRAGMA journal_mode=WAL;",
            Self::Delete => "PRAGMA journal_mode=DELETE;",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Wal => "wal",
            Self::Delete => "delete",
        }
    }
}

// ===========================================================================
// Fixture harness
// ===========================================================================

struct Fixture {
    parent: PathBuf,
    root: IsolatedFixtureRoot,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let parent = std::fs::canonicalize(std::env::temp_dir())?.join(format!(
            "orchestrator-rs-sqlite-compat-{}-{number}-{label}",
            std::process::id()
        ));
        private_dir(&parent)?;
        let root = IsolatedFixtureRoot::create_fresh(&parent)?;
        Ok(Self { parent, root })
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn learnings_database(&self) -> PathBuf {
        self.path().join(".alluka").join("learnings.db")
    }

    fn metrics_database(&self) -> PathBuf {
        self.path().join("metrics.db")
    }

    /// Builds a Go-shaped `learnings.db` at one point of the matrix.
    fn seed_learnings(&self, shape: Shape, journal: Journal, version: i64) -> TestResult {
        let directory = self.path().join(".alluka");
        private_dir(&directory)?;
        let connection = Connection::open(self.learnings_database())?;
        connection.execute_batch(journal.pragma())?;
        connection.execute_batch(SCHEMA_VERSION_DDL)?;
        connection.execute(
            "INSERT INTO schema_version (version)
             SELECT ?1 WHERE NOT EXISTS (SELECT 1 FROM schema_version)",
            rusqlite::params![version],
        )?;
        match shape {
            Shape::Fresh => {
                connection.execute_batch(LEARNINGS_FRESH_DDL)?;
                // Even a "fresh" database runs the migration list; Go does not
                // branch on shape, it just discards the duplicate-column error.
                apply_ignoring_duplicates(&connection, LEARNINGS_MIGRATIONS);
                apply_ignoring_duplicates(&connection, &[EMBEDDING_MIGRATION]);
            }
            Shape::Migrated => {
                connection.execute_batch(LEARNINGS_LEGACY_DDL)?;
                apply_ignoring_duplicates(&connection, LEARNINGS_MIGRATIONS);
                apply_ignoring_duplicates(&connection, &[EMBEDDING_MIGRATION]);
            }
        }
        connection.execute_batch(LEARNINGS_FTS_DDL)?;
        connection.execute_batch(LEARNINGS_INDEXES)?;
        seed_rows(&connection)?;
        Ok(())
    }

    fn adapter(&self) -> Result<GoLearningAdapter, GoAdapterError> {
        GoLearningAdapter::in_fixture(&self.root)
    }

    fn owner(&self) -> TestResult<MetricsOwner> {
        Ok(MetricsOwner::assume(
            MetricsOwnerCapability::in_fixture_boundary(
                orchestrator_app::fixture_production_boundary(&self.root)?,
            )?,
        )?)
    }
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Applies each statement, discarding exactly the failure Go discards.
///
/// Suppressing only "duplicate column name" matters: a blanket `let _ =` would
/// hide a genuinely broken migration and the matrix would stop proving anything.
fn apply_ignoring_duplicates(connection: &Connection, statements: &[&str]) {
    for statement in statements {
        match connection.execute(statement, []) {
            Ok(_) => {}
            Err(error) => assert!(
                error.to_string().contains("duplicate column name"),
                "unexpected migration failure: {error}",
            ),
        }
    }
}

/// One seeded `learnings` row. `embedding` is `None` for a row the Go embedder
/// never reached.
struct SeedRow {
    id: &'static str,
    learning_type: &'static str,
    content: &'static str,
    domain: &'static str,
    quality_score: f64,
    embedding: Option<Vec<u8>>,
}

/// Three rows, one of which has a `NULL` embedding.
fn seed_rows(connection: &Connection) -> TestResult {
    let rows = [
        SeedRow {
            id: "l-1",
            learning_type: "insight",
            content: "kernel reads are cheap",
            domain: "dev",
            quality_score: 0.90,
            embedding: Some(vec![1, 2, 3]),
        },
        SeedRow {
            id: "l-2",
            learning_type: "gotcha",
            content: "wal needs shared memory",
            domain: "dev",
            quality_score: 0.55,
            // A row the Go embedder never reached. Reading it must not error.
            embedding: None,
        },
        SeedRow {
            id: "l-3",
            learning_type: "decision",
            content: "one owner per store",
            domain: "ops",
            quality_score: 0.75,
            embedding: Some(vec![9]),
        },
    ];
    for row in rows {
        connection.execute(
            "INSERT INTO learnings
                (id, type, content, context, domain, quality_score, created_at, embedding)
             VALUES (?1, ?2, ?3, '', ?4, ?5, '2026-08-26T00:00:00Z', ?6)",
            rusqlite::params![
                row.id,
                row.learning_type,
                row.content,
                row.domain,
                row.quality_score,
                row.embedding,
            ],
        )?;
    }
    Ok(())
}

/// The set of `sqlite_schema` objects, used to prove an additive write left the
/// FTS5 table and its triggers alone.
fn schema_objects(database: &Path) -> TestResult<BTreeSet<(String, String)>> {
    let connection = Connection::open(database)?;
    let mut statement = connection.prepare(
        "SELECT type, name FROM sqlite_schema
         WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(rows)
}

fn full_page() -> LearningListQuery {
    LearningListQuery {
        domain: None,
        learning_type: None,
        include_archived: true,
        limit: 64,
    }
}

// ===========================================================================
// 1. Every shape reads identically
// ===========================================================================

#[test]
fn fresh_and_migrated_schemas_read_identically_in_both_journal_modes() -> TestResult {
    let mut readings = Vec::new();
    for shape in Shape::ALL {
        for journal in Journal::ALL {
            let label = format!("{}-{}", shape.label(), journal.label());
            let fixture = Fixture::new(&label)?;
            fixture.seed_learnings(shape, journal, 1)?;

            // The two shapes really do differ on disk; otherwise this whole
            // matrix would be one case wearing four hats.
            let column_order = learnings_column_order(&fixture.learnings_database())?;

            let adapter = fixture.adapter()?;
            assert_eq!(adapter.schema_version()?, 1, "{label}");
            let page = adapter.list(&full_page())?;
            let stats = adapter.stats()?;
            let top = adapter.top_by_quality(&TopQualityQuery {
                domain: None,
                limit: 8,
            })?;
            readings.push((label, shape, column_order, page.rows, stats, top));
        }
    }

    let fresh_columns = &readings
        .iter()
        .find(|reading| reading.1 == Shape::Fresh)
        .ok_or("a fresh reading")?
        .2;
    let migrated_columns = &readings
        .iter()
        .find(|reading| reading.1 == Shape::Migrated)
        .ok_or("a migrated reading")?
        .2;
    assert_ne!(
        fresh_columns, migrated_columns,
        "the two shapes must genuinely differ on disk, or the matrix proves nothing",
    );

    let (first_label, _, _, first_rows, first_stats, first_top) =
        readings.first().ok_or("at least one reading")?;
    for (label, _, _, rows, stats, top) in &readings {
        assert_eq!(rows, first_rows, "{label} disagrees with {first_label}");
        assert_eq!(stats, first_stats, "{label} disagrees with {first_label}");
        assert_eq!(top, first_top, "{label} disagrees with {first_label}");
    }
    assert_eq!(first_rows.len(), 3, "{first_rows:?}");
    Ok(())
}

/// One SQLite sidecar path beside a database.
fn sidecar(database: &Path, suffix: &str) -> PathBuf {
    let mut path = database.to_path_buf().into_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

/// CF-M4a-3. The state a cleanly exited Go orchestrator leaves behind.
///
/// Go opens its stores with `journal_mode=wal` and removes `-wal`/`-shm` on
/// exit, so the main file still declares WAL in its header with no shared
/// memory index beside it. Every other WAL case in this matrix is seeded by a
/// `rusqlite` connection whose `-shm` survives the seed, so the read-only
/// connection finds the index already there — which is why this boundary went
/// unexercised until `go_bidirectional_differential` crossed it with a store
/// the real oracle wrote.
///
/// The read must also leave the store alone: `immutable=1` creates nothing,
/// where the read-write open this gate's sibling used to fall back to rewrote
/// the very artifact under test.
#[test]
fn a_wal_store_without_its_sidecars_reads_and_is_left_byte_identical() -> TestResult {
    let fixture = Fixture::new("wal-sidecarless")?;
    fixture.seed_learnings(Shape::Fresh, Journal::Wal, 1)?;
    let database = fixture.learnings_database();
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(sidecar(&database, suffix));
    }
    let header = std::fs::read(&database)?;
    assert_eq!(
        header.get(18..20),
        Some(&[2, 2][..]),
        "the seeded store does not declare WAL in its file header",
    );

    let adapter = fixture.adapter()?;
    assert_eq!(adapter.schema_version()?, 1);
    let page = adapter.list(&full_page())?;
    assert_eq!(page.rows.len(), 3, "the sidecar-less WAL store read short");

    assert_eq!(
        std::fs::read(&database)?,
        header,
        "reading the store rewrote it",
    );
    for suffix in ["-wal", "-shm"] {
        assert!(
            !sidecar(&database, suffix).exists(),
            "reading the store materialised a {suffix} sidecar beside it",
        );
    }
    Ok(())
}

/// The guard on that fallback: `immutable=1` promises the bytes cannot change,
/// so the adapter only makes that promise when nothing is holding the store.
///
/// A live or crashed writer's `-wal` carries committed frames the main file
/// does not, and an `immutable=1` connection reads straight past them — a
/// stale snapshot returned as the current one. SQLite itself handles the
/// ordinary crashed-writer shape (a valid non-empty `-wal`, no `-shm`) on the
/// plain read-only path with a heap-resident index, so the fallback never runs
/// there. What the guard covers is every *other* shape where the plain path
/// fails and a sidecar is nonetheless present — here, a `-shm` with no `-wal`,
/// which is what a writer interrupted between creating its index and writing
/// its first frame leaves behind.
#[test]
fn a_wal_store_carrying_a_sidecar_is_refused_rather_than_read_past() -> TestResult {
    let fixture = Fixture::new("wal-held")?;
    fixture.seed_learnings(Shape::Fresh, Journal::Wal, 1)?;
    let database = fixture.learnings_database();
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(sidecar(&database, suffix));
    }
    // Sidecar-less, this store reads — that is the case above.
    assert_eq!(fixture.adapter()?.schema_version()?, 1);

    std::fs::write(sidecar(&database, "-shm"), b"")?;
    let refusal = fixture
        .adapter()?
        .schema_version()
        .err()
        .ok_or("a store carrying a sidecar must be refused, not read past")?;
    assert!(
        matches!(
            refusal,
            GoAdapterError::Read {
                operation: "refuse a learnings.db a writer still holds"
            }
        ),
        "unexpected refusal {refusal:?}",
    );
    Ok(())
}

fn learnings_column_order(database: &Path) -> TestResult<Vec<String>> {
    let connection = Connection::open(database)?;
    let mut statement = connection.prepare("SELECT name FROM pragma_table_info('learnings')")?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names)
}

#[test]
fn a_null_embedding_row_reads_without_error() -> TestResult {
    for shape in Shape::ALL {
        let fixture = Fixture::new(&format!("null-embedding-{}", shape.label()))?;
        fixture.seed_learnings(shape, Journal::Wal, 1)?;

        // The row is genuinely NULL in the column the Go embedder writes.
        let connection = Connection::open(fixture.learnings_database())?;
        let nulls: i64 = connection.query_row(
            "SELECT count(*) FROM learnings WHERE embedding IS NULL",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(nulls, 1, "{}", shape.label());
        drop(connection);

        let rows = fixture.adapter()?.list(&full_page())?.rows;
        assert_eq!(rows.len(), 3, "{}", shape.label());
        assert!(
            rows.iter().any(|row| row.id == "l-2"),
            "the un-embedded row must still be readable",
        );
    }
    Ok(())
}

// ===========================================================================
// 2. A too-new schema fails closed
// ===========================================================================

#[test]
fn a_schema_version_above_the_supported_maximum_is_refused_not_partially_read() -> TestResult {
    let too_new = i64::from(MAX_SUPPORTED_LEARNING_SCHEMA_VERSION) + 1;
    for shape in Shape::ALL {
        let fixture = Fixture::new(&format!("too-new-{}", shape.label()))?;
        fixture.seed_learnings(shape, Journal::Wal, too_new)?;
        let adapter = fixture.adapter()?;

        // Every read path refuses, not just the one that happens to check.
        // A partial read is the failure mode `db.go:105-107` exists to prevent.
        let version = adapter.schema_version();
        let list = adapter.list(&full_page());
        let top = adapter.top_by_quality(&TopQualityQuery {
            domain: None,
            limit: 8,
        });
        for (path, outcome) in [
            ("schema_version", version.map(|_| ())),
            ("list", list.map(|_| ())),
            ("top_by_quality", top.map(|_| ())),
        ] {
            assert!(
                matches!(
                    outcome,
                    Err(GoAdapterError::SchemaTooNew {
                        found,
                        supported,
                    }) if found == u32::try_from(too_new).unwrap_or_default()
                        && supported == MAX_SUPPORTED_LEARNING_SCHEMA_VERSION
                ),
                "{}/{path} must refuse a too-new schema",
                shape.label(),
            );
        }
    }
    Ok(())
}

#[test]
fn every_supported_schema_version_reads() -> TestResult {
    // The matrix is over "each `schema_version` the Go code supports", which is
    // currently just 1. Deriving the range from the constant rather than
    // hard-coding it means a future bump widens this test instead of silently
    // leaving the new version uncovered.
    for version in 1..=MAX_SUPPORTED_LEARNING_SCHEMA_VERSION {
        for shape in Shape::ALL {
            let fixture = Fixture::new(&format!("v{version}-{}", shape.label()))?;
            fixture.seed_learnings(shape, Journal::Wal, i64::from(version))?;
            let adapter = fixture.adapter()?;
            assert_eq!(adapter.schema_version()?, version);
            assert_eq!(adapter.list(&full_page())?.rows.len(), 3);
        }
    }
    Ok(())
}

// ===========================================================================
// 3. An additive write leaves the FTS5 machinery intact
// ===========================================================================

#[test]
fn a_declared_additive_write_leaves_the_fts_table_and_triggers_intact() -> TestResult {
    for shape in Shape::ALL {
        let fixture = Fixture::new(&format!("fts-{}", shape.label()))?;
        fixture.seed_learnings(shape, Journal::Wal, 1)?;
        let database = fixture.learnings_database();
        let before = schema_objects(&database)?;
        assert!(
            before.contains(&("table".to_owned(), "learnings_fts".to_owned())),
            "the external-content table must exist to begin with",
        );
        for trigger in ["learnings_ai", "learnings_ad", "learnings_au"] {
            assert!(
                before.contains(&("trigger".to_owned(), trigger.to_owned())),
                "{trigger} must exist to begin with",
            );
        }

        let grant = AdditiveFixtureGrant::in_fixture(&fixture.root)?;
        let appender = GoLearningAppender::for_grant(&grant)?;
        let before_digest = fixture.adapter()?.row_set_digest()?;
        appender.insert_new(
            &grant,
            &[NewLearningRow {
                id: "l-added".to_owned(),
                learning_type: "insight".to_owned(),
                content: "additive writes must not disturb the index".to_owned(),
                context: String::new(),
                domain: "dev".to_owned(),
                created_at: "2026-08-26T01:00:00Z".to_owned(),
            }],
        )?;

        let after = schema_objects(&database)?;
        assert_eq!(
            before,
            after,
            "{}: an additive write must not change any schema object",
            shape.label(),
        );

        // The `learnings_ai` trigger must actually have fired: an
        // external-content FTS5 table that silently stops indexing is worse
        // than one that errors.
        let connection = Connection::open(&database)?;
        let indexed: i64 = connection.query_row(
            "SELECT count(*) FROM learnings_fts WHERE learnings_fts MATCH 'additive'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            indexed,
            1,
            "{}: the insert trigger must fire",
            shape.label()
        );
        drop(connection);

        // And the pre-existing rows are untouched.
        let adapter = fixture.adapter()?;
        let after_digest = adapter.row_set_digest()?;
        assert_eq!(
            before_digest.digest(),
            after_digest.restricted_to(before_digest.ids(), &adapter.digest_components()?),
            "{}: pre-existing rows must be byte-identical",
            shape.label(),
        );
        assert_eq!(adapter.list(&full_page())?.rows.len(), 4);
    }
    Ok(())
}

// ===========================================================================
// 4. metrics.db before and after its migrations
// ===========================================================================

#[test]
fn a_pre_migration_metrics_db_is_brought_forward_and_read() -> TestResult {
    let fixture = Fixture::new("metrics-legacy")?;
    let database = fixture.metrics_database();
    {
        let connection = Connection::open(&database)?;
        connection.execute_batch(METRICS_LEGACY_DDL)?;
        // A database of this vintage genuinely lacks the later machinery.
        for absent in ["quota_snapshots", "usage_snapshots"] {
            let count: i64 = connection.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                rusqlite::params![absent],
                |row| row.get(0),
            )?;
            assert_eq!(count, 0, "{absent} must be absent before the migration");
        }
        let worker_name: i64 = connection.query_row(
            "SELECT count(*) FROM pragma_table_info('phases') WHERE name = 'worker_name'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(worker_name, 0, "phases.worker_name must be absent");
    }

    // Assuming ownership applies Go's own migration list.
    let owner = fixture.owner()?;
    {
        let connection = Connection::open(&database)?;
        for expected in ["quota_snapshots", "usage_snapshots"] {
            let count: i64 = connection.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                rusqlite::params![expected],
                |row| row.get(0),
            )?;
            assert_eq!(count, 1, "{expected} must exist after the migration");
        }
        let index: i64 = connection.query_row(
            "SELECT count(*) FROM sqlite_schema
             WHERE type = 'index' AND name = 'idx_phases_worker_name'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(index, 1, "idx_phases_worker_name must exist");
        for column in [
            "worker_name",
            "parsed_skills",
            "tokens_cache_creation",
            "tokens_cache_read",
            "error_message",
            "selection_method",
        ] {
            let count: i64 = connection.query_row(
                "SELECT count(*) FROM pragma_table_info('phases') WHERE name = ?1",
                rusqlite::params![column],
                |row| row.get(0),
            )?;
            assert_eq!(count, 1, "phases.{column} must exist after the migration");
        }
    }

    // And the store is writable and readable through the owner afterwards.
    let mission = MissionId::new("mission-legacy-metrics")?;
    let intent = PhaseMetricIntent::new(mission, "phase-1", 1, reaped_witness()?);
    owner.record_phase(&intent)?;
    let row = owner
        .phase_row("mission-legacy-metrics", "phase-1")?
        .ok_or("the phase must be readable back")?;
    assert!(row.parsed_skills.is_empty());
    Ok(())
}

#[test]
fn a_post_migration_metrics_db_is_adopted_without_further_change() -> TestResult {
    let fixture = Fixture::new("metrics-current")?;
    // First assumption creates and migrates the store.
    let owner = fixture.owner()?;
    let mission = MissionId::new("mission-current-metrics")?;
    owner.record_phase(&PhaseMetricIntent::new(
        mission,
        "phase-1",
        1,
        reaped_witness()?,
    ))?;
    let before = schema_objects(&fixture.metrics_database())?;
    drop(owner);

    // Second assumption over the same file must be a no-op on the schema.
    let owner = fixture.owner()?;
    let after = schema_objects(&fixture.metrics_database())?;
    assert_eq!(
        before, after,
        "re-adopting a current store must change nothing"
    );
    assert_eq!(
        owner.recorded_phase_names("mission-current-metrics")?,
        vec!["phase-1".to_owned()],
        "the existing row must survive re-adoption",
    );
    Ok(())
}

// ===========================================================================
// Process-group witness
// ===========================================================================

/// Spawns a short-lived process in its own group, reaps it, and returns proof
/// the group is gone.
///
/// `PhaseMetricIntent` cannot be built without one, so even a schema test needs
/// a real reaped process to exercise the metrics write path.
fn reaped_witness() -> TestResult<orchestrator_app::ExactProcessGroupAbsence> {
    use orchestrator_app::{
        KernelProcessIdentity, RecordedProcessIdentityStatus, inspect_recorded_process_identity,
    };
    use std::os::unix::process::CommandExt;

    let mut command = std::process::Command::new(std::env::current_exe()?);
    scrub_provider_environment(&mut command);
    command
        .args(["--exact", "witness_entrypoint", "--nocapture"])
        .env("NANIKA_B3_COMPAT_WITNESS", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    command.process_group(0);
    let mut child = command.spawn()?;
    let pid = child.id();
    {
        use std::io::BufRead;
        let stdout = child.stdout.take().ok_or("witness stdout")?;
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return Err("witness exited before announcing readiness".into());
            }
            // libtest with `--test-threads=1` prints `test <name> ... ` and no
            // newline *before* running the test, so the child's readiness token lands
            // at the end of that progress line rather than on a line of its own.
            // Comparing the whole line therefore never matches under the verification
            // lease, which sets RUST_TEST_THREADS=1, and the handshake deadlocks: the
            // parent waits for a line it will never see while the child waits for the
            // stdin EOF the parent sends only after it. The last whitespace-separated
            // token is the token in both of libtest's printing modes.
            if line.split_whitespace().next_back() == Some("ready") {
                break;
            }
        }
    }
    let identity = KernelProcessIdentity::observe(pid, pid)?;
    drop(child.stdin.take());
    child.wait()?;
    match inspect_recorded_process_identity(
        identity.pid(),
        identity.process_group_id(),
        identity.process_start_identity(),
    )? {
        RecordedProcessIdentityStatus::ExactGroupAbsent(absence) => Ok(absence),
        other => Err(format!("expected a reaped group, observed {other:?}").into()),
    }
}

/// Re-exec entry point for [`reaped_witness`].
#[test]
fn witness_entrypoint() {
    if std::env::var("NANIKA_B3_COMPAT_WITNESS").is_err() {
        return;
    }
    use std::io::{BufRead, Write};
    println!("ready");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    std::process::exit(0);
}

/// Provider and credential variables scrubbed from every helper subprocess.
///
/// Verbatim from `CORE-100-VERIFICATION-GAPS.md` lines 21-37. The helper reads
/// none of them, but "no credential is touched" is an absolute: a child holding
/// live provider keys in its environment is one careless `Command` away from
/// leaking them, and `env_remove` costs nothing to keep that impossible.
const SCRUBBED_PROVIDER_VARIABLES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "GEMINI_API_KEY",
    "ELEVENLABS_API_KEY",
    "ALLUKA_AUTH_FILE",
    "CLAUDE_CREDENTIALS_DIR",
    "CLAUDE_CREDENTIALS_FILE",
    "CODEX_PATH",
    "NANIKA_HERMETIC_PROCESS_CANARY",
];

/// Removes every provider variable from a helper command's environment.
fn scrub_provider_environment(command: &mut std::process::Command) -> &mut std::process::Command {
    for name in SCRUBBED_PROVIDER_VARIABLES {
        command.env_remove(name);
    }
    command
}
