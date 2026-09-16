//! Advisory changed-file collection and cross-mission claim registry.
//!
//! [`claim_changed_files`] is a faithful port of the Go oracle's
//! `ClaimChangedFiles`: the sorted, deduplicated union of committed
//! branch-diff files and any staged, unstaged, or untracked files currently
//! present in the worktree.
//!
//! [`ClaimsDb`] is a faithful port of Go's `internal/claims` package (`db.go`):
//! an advisory SQLite registry so parallel missions targeting the same
//! repository can detect potential edit conflicts. Claims are warnings, not
//! locks — a mission that finds conflicts prints them and continues. The
//! registry lives in a `file_claims` table added to the shared
//! `~/.alluka/learnings.db`.
//!
//! This module opens its own advisory-locked `rusqlite::Connection` (Go's
//! `busy_timeout=5000`), matching the Go `sql.Open` call site 1:1. The
//! `file_claims` table lives inside the shared `learnings.db`; ADR-0001's
//! ownership table grants it an explicit, narrowly-scoped exception from
//! storage-actor ownership (advisory Git worktree-claim coordination, disjoint
//! from the storage actor's tables, coordinated by SQLite file-level locking).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, params_from_iter};
use thiserror::Error;

use crate::GitError;
use crate::repo::{changed_files, name_only};
use crate::time_util::utc_rfc3339_now;

/// Returns the sorted union of committed branch-diff files (`base...head` run in
/// `repo_root`) and any staged, unstaged, or untracked files in `worktree`.
///
/// Pass `None` for `repo_root`/`base`/`head` to skip the committed-diff source,
/// and `None` for `worktree` to skip the worktree sources.
pub fn claim_changed_files(
    repo_root: Option<&Path>,
    worktree: Option<&Path>,
    base: Option<&str>,
    head: Option<&str>,
) -> Result<Vec<String>, GitError> {
    let mut files: BTreeSet<String> = BTreeSet::new();

    if let (Some(repo_root), Some(base), Some(head)) = (repo_root, base, head) {
        for file in changed_files(repo_root, base, head)? {
            if !file.is_empty() {
                files.insert(file);
            }
        }
    }

    if let Some(worktree) = worktree {
        for file in name_only(worktree, &["git", "diff", "--name-only", "--cached"])? {
            if !file.is_empty() {
                files.insert(file);
            }
        }
        for file in name_only(worktree, &["git", "diff", "--name-only"])? {
            if !file.is_empty() {
                files.insert(file);
            }
        }
        for file in name_only(
            worktree,
            &["git", "ls-files", "--others", "--exclude-standard"],
        )? {
            if !file.is_empty() {
                files.insert(file);
            }
        }
    }

    Ok(files.into_iter().collect())
}

/// Env var checked first for the orchestrator config directory (highest
/// priority). Matches Go's `config.EnvVar`.
const CONFIG_DIR_ENV_VAR: &str = "ORCHESTRATOR_CONFIG_DIR";
/// Unified Nanika config home env var. Matches Go's `config.EnvVarAllukaHome`.
const ALLUKA_HOME_ENV_VAR: &str = "ALLUKA_HOME";
/// Legacy cross-tool base directory env var. Matches Go's `config.EnvVarViaHome`.
const VIA_HOME_ENV_VAR: &str = "VIA_HOME";
/// Default config directory name under `$HOME`. Matches Go's `config.DirName`.
const CONFIG_DIR_NAME: &str = ".alluka";
/// Pre-Nanika config directory name. Matches Go's `config.DirNameLegacy`.
const CONFIG_DIR_NAME_LEGACY: &str = ".via";
/// Filename of the shared learnings database the claim registry lives in.
const LEARNINGS_DB_FILE_NAME: &str = "learnings.db";

/// Failures raised by the [`ClaimsDb`] advisory claim registry.
///
/// One variant per distinct Go `fmt.Errorf("...: %w", err)` wrap site in
/// `internal/claims/db.go`, per house convention (see `GitError`).
#[derive(Debug, Error)]
pub enum ClaimsDbError {
    /// `os.UserHomeDir()` / config-dir env resolution failed.
    #[error("get config dir: {source}")]
    ConfigDir {
        #[source]
        source: std::io::Error,
    },
    /// `os.MkdirAll(base, 0700)` failed.
    #[error("create config dir: {source}")]
    CreateConfigDir {
        #[source]
        source: std::io::Error,
    },
    /// `sql.Open` (connection open or pragma setup) failed.
    #[error("open db: {source}")]
    Open {
        #[source]
        source: rusqlite::Error,
    },
    /// `CREATE TABLE IF NOT EXISTS file_claims` failed.
    #[error("init schema: {source}")]
    InitSchema {
        #[source]
        source: rusqlite::Error,
    },
    /// `db.Begin()` failed.
    #[error("begin tx: {source}")]
    BeginTx {
        #[source]
        source: rusqlite::Error,
    },
    /// `tx.Prepare(...)` failed.
    #[error("prepare stmt: {source}")]
    PrepareStmt {
        #[source]
        source: rusqlite::Error,
    },
    /// `tx.Prepare(...)` failed for the release-then-insert path.
    #[error("prepare insert: {source}")]
    PrepareInsert {
        #[source]
        source: rusqlite::Error,
    },
    /// `stmt.Exec(f, missionID, repoRoot, now)` failed for one file.
    #[error("insert claim for {file:?}: {source}")]
    InsertClaim {
        file: String,
        #[source]
        source: rusqlite::Error,
    },
    /// `tx.Exec(...)` releasing existing active claims failed.
    #[error("release existing claims: {source}")]
    ReleaseExistingClaims {
        #[source]
        source: rusqlite::Error,
    },
    /// `tx.Commit()` failed.
    #[error("commit tx: {source}")]
    CommitTx {
        #[source]
        source: rusqlite::Error,
    },
    /// `d.db.Query(query, args...)` failed.
    #[error("query conflicts: {source}")]
    QueryConflicts {
        #[source]
        source: rusqlite::Error,
    },
    /// `rows.Scan(&c.FilePath, &c.MissionID)` failed.
    #[error("scan conflict row: {source}")]
    ScanConflictRow {
        #[source]
        source: rusqlite::Error,
    },
    /// `d.db.Exec(...)` releasing all active claims for a mission failed.
    #[error("release claims: {source}")]
    ReleaseClaims {
        #[source]
        source: rusqlite::Error,
    },
    /// `d.db.Exec(\`DELETE FROM file_claims ...\`)` failed.
    #[error("purge claims: {source}")]
    PurgeClaims {
        #[source]
        source: rusqlite::Error,
    },
    /// `d.db.Close()` failed.
    #[error("close db: {source}")]
    Close {
        #[source]
        source: rusqlite::Error,
    },
}

/// Describes a file already claimed by another active mission.
///
/// Matches Go's `Conflict{FilePath, MissionID}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub file_path: String,
    pub mission_id: String,
}

/// Wraps the SQLite connection for advisory claim operations.
///
/// Matches Go's `DB{db *sql.DB}`. Each `ClaimsDb` owns its own connection —
/// this diverges from the runtime-store storage-actor pattern used for
/// `learnings.db`/`metrics.db`/`chat.db` elsewhere in the Rust workspace;
/// see the module doc comment (ADR-0001 claims.db ownership exception).
pub struct ClaimsDb {
    conn: Connection,
}

/// Opens (or creates) the learnings.db and ensures the `file_claims` table
/// exists. Pass `None` to use the default `~/.alluka/learnings.db`.
///
/// Matches Go's `OpenDB(dbPath string) (*DB, error)`; the empty-string
/// sentinel for "use the default path" becomes `None` in Rust.
pub fn open_claims_db(db_path: Option<&Path>) -> Result<ClaimsDb, ClaimsDbError> {
    let owned_default;
    let path = match db_path {
        Some(path) => path,
        None => {
            let base =
                default_config_dir().map_err(|source| ClaimsDbError::ConfigDir { source })?;
            create_config_dir_matching_go_mkdirall(&base)
                .map_err(|source| ClaimsDbError::CreateConfigDir { source })?;
            owned_default = base.join(LEARNINGS_DB_FILE_NAME);
            &owned_default
        }
    };

    let conn = Connection::open(path).map_err(|source| ClaimsDbError::Open { source })?;
    // Matches Go's `?_journal_mode=WAL&_busy_timeout=5000` connection-string
    // pragmas (`db.go:46`).
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|source| ClaimsDbError::Open { source })?;
    conn.pragma_update(None, "busy_timeout", 5000_i64)
        .map_err(|source| ClaimsDbError::Open { source })?;

    init_schema(&conn).map_err(|source| ClaimsDbError::InitSchema { source })?;

    Ok(ClaimsDb { conn })
}

/// Creates `path` (and any missing parents) if it does not already exist,
/// then applies the private (0700) mode only when `path` itself did not
/// already exist beforehand.
///
/// Matches Go's `os.MkdirAll(base, 0700)` (`db.go:40`): `MkdirAll` is a
/// documented no-op — including no mode change — when the directory already
/// exists; it applies `perm` only to directories it actually creates. A
/// bare unconditional chmod after `create_dir_all` would strip permissions
/// from a directory another process/tool created with broader access,
/// which `~/.alluka` (a shared config root) can be.
fn create_config_dir_matching_go_mkdirall(path: &Path) -> std::io::Result<()> {
    let preexisted = path.exists();
    std::fs::create_dir_all(path)?;
    if !preexisted {
        set_private_dir_mode(path)?;
    }
    Ok(())
}

fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS file_claims (
            file_path   TEXT NOT NULL,
            mission_id  TEXT NOT NULL,
            repo_root   TEXT NOT NULL,
            claimed_at  DATETIME NOT NULL,
            released_at DATETIME,
            PRIMARY KEY (file_path, mission_id)
        )",
    )
}

/// Resolves the orchestrator base config directory.
///
/// Faithful port of Go's `internal/config.Dir()`. Priority (highest to
/// lowest): `ORCHESTRATOR_CONFIG_DIR` env var; `ALLUKA_HOME` env var;
/// `VIA_HOME` env var joined with `orchestrator`; `~/.alluka` if it exists;
/// `~/.via` as the legacy fallback.
///
/// Scoped locally to this module (rather than a shared `config` crate/module)
/// per this port's single-package worktree boundary — flagged in the port
/// report as a coupling candidate rather than reached for silently.
fn default_config_dir() -> std::io::Result<PathBuf> {
    if let Ok(dir) = std::env::var(CONFIG_DIR_ENV_VAR) {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    if let Ok(dir) = std::env::var(ALLUKA_HOME_ENV_VAR) {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    if let Ok(dir) = std::env::var(VIA_HOME_ENV_VAR) {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir).join("orchestrator"));
        }
    }
    let home = home_dir()?;
    let alluka = home.join(CONFIG_DIR_NAME);
    if alluka.exists() {
        return Ok(alluka);
    }
    Ok(home.join(CONFIG_DIR_NAME_LEGACY))
}

/// Minimal `os.UserHomeDir()` equivalent: reads `$HOME` (Unix) or
/// `%USERPROFILE%` (Windows), matching the Go standard library's own
/// resolution order on each platform.
fn home_dir() -> std::io::Result<PathBuf> {
    #[cfg(unix)]
    let var_name = "HOME";
    #[cfg(windows)]
    let var_name = "USERPROFILE";
    #[cfg(not(any(unix, windows)))]
    let var_name = "HOME";

    std::env::var(var_name).map(PathBuf::from).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{var_name} is not set"),
        )
    })
}

/// Sets the config directory to mode 0700 (owner-only) on Unix, matching
/// Go's `os.MkdirAll(base, 0700)`. Best-effort private on non-Unix, matching
/// `write_private_file`'s discipline elsewhere in this crate.
fn set_private_dir_mode(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

impl ClaimsDb {
    /// Records advisory claims for all files in `files` under `mission_id`.
    /// Existing claims for the same `(file_path, mission_id)` pair are
    /// replaced so that re-runs do not accumulate stale rows.
    ///
    /// Matches Go's `(d *DB) ClaimFiles(missionID, repoRoot string, files
    /// []string) error`.
    pub fn claim_files(
        &mut self,
        mission_id: &str,
        repo_root: &str,
        files: &[String],
    ) -> Result<(), ClaimsDbError> {
        if files.is_empty() {
            return Ok(());
        }

        let now = utc_rfc3339_now();

        let tx = self
            .conn
            .transaction()
            .map_err(|source| ClaimsDbError::BeginTx { source })?;

        {
            let mut stmt = tx
                .prepare(
                    "INSERT OR REPLACE INTO file_claims \
                     (file_path, mission_id, repo_root, claimed_at, released_at) \
                     VALUES (?, ?, ?, ?, NULL)",
                )
                .map_err(|source| ClaimsDbError::PrepareStmt { source })?;

            for file in files {
                stmt.execute((file.as_str(), mission_id, repo_root, now.as_str()))
                    .map_err(|source| ClaimsDbError::InsertClaim {
                        file: file.clone(),
                        source,
                    })?;
            }
        }

        tx.commit()
            .map_err(|source| ClaimsDbError::CommitTx { source })
    }

    /// Returns active claims on any of `files` by missions other than
    /// `mission_id` that share the same `repo_root`.
    ///
    /// Matches Go's `(d *DB) CheckConflicts(missionID, repoRoot string,
    /// files []string) ([]Conflict, error)`.
    pub fn check_conflicts(
        &self,
        mission_id: &str,
        repo_root: &str,
        files: &[String],
    ) -> Result<Vec<Conflict>, ClaimsDbError> {
        if files.is_empty() {
            return Ok(Vec::new());
        }

        let placeholders = vec!["?"; files.len()].join(",");
        let query = format!(
            "SELECT file_path, mission_id FROM file_claims \
             WHERE mission_id != ? \
               AND repo_root   =  ? \
               AND released_at IS NULL \
               AND file_path IN ({placeholders})"
        );

        let mut stmt = self
            .conn
            .prepare(&query)
            .map_err(|source| ClaimsDbError::QueryConflicts { source })?;

        let params: Vec<&str> = std::iter::once(mission_id)
            .chain(std::iter::once(repo_root))
            .chain(files.iter().map(String::as_str))
            .collect();

        let rows = stmt
            .query_map(params_from_iter(params), |row| {
                Ok(Conflict {
                    file_path: row.get(0)?,
                    mission_id: row.get(1)?,
                })
            })
            .map_err(|source| ClaimsDbError::QueryConflicts { source })?;

        let mut conflicts = Vec::new();
        for row in rows {
            conflicts.push(row.map_err(|source| ClaimsDbError::ScanConflictRow { source })?);
        }
        Ok(conflicts)
    }

    /// Atomically replaces all active claims for `mission_id` with per-file
    /// claims for `files`. Any file previously claimed but absent from
    /// `files` has its claim released, preventing stale rows from prior runs
    /// from accumulating. If `files` is empty the call is equivalent to
    /// [`ClaimsDb::release_all`].
    ///
    /// Matches Go's `(d *DB) UpdateFileClaimsWithFiles(missionID, repoRoot
    /// string, files []string) error`.
    pub fn update_file_claims_with_files(
        &mut self,
        mission_id: &str,
        repo_root: &str,
        files: &[String],
    ) -> Result<(), ClaimsDbError> {
        let now = utc_rfc3339_now();

        let tx = self
            .conn
            .transaction()
            .map_err(|source| ClaimsDbError::BeginTx { source })?;

        // Release all existing active claims for this mission so that files
        // dropped from the new set are not left with stale active claims.
        tx.execute(
            "UPDATE file_claims SET released_at = ? \
             WHERE mission_id = ? AND released_at IS NULL",
            (now.as_str(), mission_id),
        )
        .map_err(|source| ClaimsDbError::ReleaseExistingClaims { source })?;

        if !files.is_empty() {
            let mut stmt = tx
                .prepare(
                    "INSERT OR REPLACE INTO file_claims \
                     (file_path, mission_id, repo_root, claimed_at, released_at) \
                     VALUES (?, ?, ?, ?, NULL)",
                )
                .map_err(|source| ClaimsDbError::PrepareInsert { source })?;

            for file in files {
                stmt.execute((file.as_str(), mission_id, repo_root, now.as_str()))
                    .map_err(|source| ClaimsDbError::InsertClaim {
                        file: file.clone(),
                        source,
                    })?;
            }
        }

        tx.commit()
            .map_err(|source| ClaimsDbError::CommitTx { source })
    }

    /// Marks all active claims for `mission_id` as released.
    ///
    /// Matches Go's `(d *DB) ReleaseAll(missionID string) error`.
    pub fn release_all(&self, mission_id: &str) -> Result<(), ClaimsDbError> {
        let now = utc_rfc3339_now();
        self.conn
            .execute(
                "UPDATE file_claims SET released_at = ? \
                 WHERE mission_id = ? AND released_at IS NULL",
                (now.as_str(), mission_id),
            )
            .map_err(|source| ClaimsDbError::ReleaseClaims { source })?;
        Ok(())
    }

    /// Deletes all claim rows (released or not) whose `claimed_at` is older
    /// than `max_age`. Returns the number of rows deleted.
    ///
    /// Matches Go's `(d *DB) PurgeStaleClaims(maxAge time.Duration) (int64,
    /// error)`.
    pub fn purge_stale_claims(&self, max_age: Duration) -> Result<i64, ClaimsDbError> {
        let cutoff = rfc3339_now_minus(max_age);
        let changed = self
            .conn
            .execute(
                "DELETE FROM file_claims WHERE claimed_at < ?",
                (cutoff.as_str(),),
            )
            .map_err(|source| ClaimsDbError::PurgeClaims { source })?;
        Ok(changed as i64)
    }

    /// Closes the underlying database connection.
    ///
    /// Matches Go's `(d *DB) Close() error`.
    pub fn close(self) -> Result<(), ClaimsDbError> {
        self.conn
            .close()
            .map_err(|(_, source)| ClaimsDbError::Close { source })
    }
}

/// Formats `SystemTime::now() - max_age` as RFC3339 with a `Z` offset.
///
/// Matches Go's `time.Now().Add(-maxAge).UTC().Format(time.RFC3339)`
/// (`db.go:203`): Go subtracts at full nanosecond precision and only
/// truncates to whole seconds at format time (a floor of the *difference*).
/// Nanosecond precision is kept throughout via `i128` and only floor-divided
/// to seconds at the very end (`div_euclid`, so it floors correctly even for
/// negative results) to match that ordering exactly — truncating `now` or
/// `max_age` to whole seconds *before* subtracting would floor a second
/// early for some sub-second-straddling inputs. Duplicates the small UTC
/// civil-from-days conversion already implemented as a private helper in
/// `time_util.rs` because this port's ownership scope is limited to
/// `claims.rs` — flagged in the port report as a coupling candidate
/// (`time_util`'s helpers could be made `pub(crate)` to remove the
/// duplication) rather than reached for silently.
fn rfc3339_now_minus(max_age: Duration) -> String {
    let now_nanos: i128 = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as i128,
        Err(earlier) => -(earlier.duration().as_nanos() as i128),
    };
    let max_age_nanos = max_age.as_nanos() as i128;
    let cutoff_nanos = now_nanos.saturating_sub(max_age_nanos);
    let cutoff_secs = cutoff_nanos.div_euclid(1_000_000_000);
    let cutoff_secs = i64::try_from(cutoff_secs).unwrap_or(if cutoff_secs.is_negative() {
        i64::MIN
    } else {
        i64::MAX
    });
    format_epoch_secs_rfc3339(cutoff_secs)
}

fn format_epoch_secs_rfc3339(epoch_secs: i64) -> String {
    let (year, month, day, hour, minute, second) = broken_down_utc(epoch_secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn broken_down_utc(epoch_secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = epoch_secs.div_euclid(86_400);
    let secs_of_day = epoch_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = (secs_of_day / 3600) as u32;
    let minute = ((secs_of_day % 3600) / 60) as u32;
    let second = (secs_of_day % 60) as u32;
    (year, month, day, hour, minute, second)
}

/// Howard Hinnant's civil-from-days algorithm; identical to the private copy
/// in `time_util.rs` (see `rfc3339_now_minus`'s doc comment).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

#[cfg(test)]
mod claims_db_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    static NONCE: AtomicU64 = AtomicU64::new(0);

    /// RAII guard for a unique scratch directory, removed best-effort on drop.
    /// No `tempfile`-equivalent crate is a workspace dependency, so this
    /// mirrors the minimal unique-path-under-`env::temp_dir()` pattern used
    /// for the same purpose elsewhere in the Rust workspace.
    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(label: &str) -> std::io::Result<Self> {
            let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "orchestrator-git-claims-test-{label}-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path)?;
            Ok(Self { path })
        }

        fn db_path(&self) -> PathBuf {
            self.path.join("test.db")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn open_test_db(dir: &TestDir) -> TestResult<ClaimsDb> {
        Ok(open_claims_db(Some(&dir.db_path()))?)
    }

    fn files(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    /// Matches Go's `TestClaimReleaseCycle`.
    #[test]
    fn claim_release_cycle() -> TestResult {
        let dir = TestDir::new("claim-release-cycle")?;
        let mut db = open_test_db(&dir)?;
        let claimed = files(&["main.go", "internal/foo/bar.go"]);

        db.claim_files("mission-1", "/repo", &claimed)?;

        // Same mission sees no conflicts with itself.
        let conflicts = db.check_conflicts("mission-1", "/repo", &claimed)?;
        assert_eq!(conflicts.len(), 0, "expected no self-conflicts");

        // A different mission should see both files as conflicted.
        let conflicts = db.check_conflicts("mission-2", "/repo", &claimed)?;
        assert_eq!(conflicts.len(), 2, "expected 2 conflicts");

        // Release mission-1's claims.
        db.release_all("mission-1")?;

        // Now mission-2 sees no conflicts.
        let conflicts = db.check_conflicts("mission-2", "/repo", &claimed)?;
        assert_eq!(conflicts.len(), 0, "expected 0 conflicts after release");
        Ok(())
    }

    /// Matches Go's `TestConflictDetection`.
    #[test]
    fn conflict_detection() -> TestResult {
        let dir = TestDir::new("conflict-detection")?;
        let mut db = open_test_db(&dir)?;
        let all = files(&["a.go", "b.go", "c.go"]);

        // mission-1 claims a.go and b.go only.
        db.claim_files("mission-1", "/repo", &all[..2])?;

        // mission-2 wants all three -- only a.go and b.go should conflict.
        let conflicts = db.check_conflicts("mission-2", "/repo", &all)?;
        assert_eq!(conflicts.len(), 2, "expected 2 conflicts (a.go, b.go)");
        for conflict in &conflicts {
            assert_eq!(conflict.mission_id, "mission-1");
        }
        Ok(())
    }

    /// Matches Go's `TestStaleClaims`.
    #[test]
    fn stale_claims() -> TestResult {
        let dir = TestDir::new("stale-claims")?;
        let db = open_test_db(&dir)?;

        // Insert an artificially old claim directly, mirroring Go's raw
        // `db.db.Exec(...)` test access to the unexported connection.
        db.conn.execute(
            "INSERT INTO file_claims (file_path, mission_id, repo_root, claimed_at, released_at) \
             VALUES ('old.go', 'old-mission', '/repo', '2020-01-01T00:00:00Z', NULL)",
            (),
        )?;

        let purged = db.purge_stale_claims(Duration::from_secs(7 * 24 * 60 * 60))?;
        assert_eq!(purged, 1, "expected 1 purged");

        // A recent claim should not be purged.
        let mut db = db;
        db.claim_files("new-mission", "/repo", &files(&["new.go"]))?;
        let purged = db.purge_stale_claims(Duration::from_secs(7 * 24 * 60 * 60))?;
        assert_eq!(purged, 0, "expected 0 purged for recent claim");
        Ok(())
    }

    /// Matches Go's `TestDifferentRepos`.
    #[test]
    fn different_repos() -> TestResult {
        let dir = TestDir::new("different-repos")?;
        let mut db = open_test_db(&dir)?;

        db.claim_files("mission-1", "/repo-a", &files(&["main.go"]))?;

        // Same file path but different repo -- no conflict expected.
        let conflicts = db.check_conflicts("mission-2", "/repo-b", &files(&["main.go"]))?;
        assert_eq!(
            conflicts.len(),
            0,
            "expected 0 conflicts across different repos"
        );
        Ok(())
    }

    /// Matches Go's `TestReleaseIdempotent`.
    #[test]
    fn release_idempotent() -> TestResult {
        let dir = TestDir::new("release-idempotent")?;
        let mut db = open_test_db(&dir)?;

        db.claim_files("m1", "/repo", &files(&["x.go"]))?;
        db.release_all("m1")?;
        // Second release should be a no-op, not an error.
        db.release_all("m1")?;
        Ok(())
    }

    /// Matches Go's `TestClaimFilesEmpty`.
    #[test]
    fn claim_files_empty() -> TestResult {
        let dir = TestDir::new("claim-files-empty")?;
        let mut db = open_test_db(&dir)?;

        // Should be a no-op, not an error.
        db.claim_files("m1", "/repo", &[])?;
        let conflicts = db.check_conflicts("m2", "/repo", &[])?;
        assert_eq!(
            conflicts.len(),
            0,
            "expected no conflicts for empty file list"
        );
        Ok(())
    }

    /// Matches Go's `TestUpdateFileClaimsWithFiles`.
    #[test]
    fn update_file_claims_with_files() -> TestResult {
        let dir = TestDir::new("update-file-claims")?;
        let mut db = open_test_db(&dir)?;

        // Initial repo-root marker claim.
        db.claim_files("m1", "/repo", &files(&["."]))?;

        // Replace with per-file claims -- stale "." must be released.
        let updated = files(&["a.go", "b.go"]);
        db.update_file_claims_with_files("m1", "/repo", &updated)?;

        // "." must no longer conflict -- it was released.
        let conflicts = db.check_conflicts("m2", "/repo", &files(&["."]))?;
        assert_eq!(conflicts.len(), 0, "expected stale '.' claim released");

        // a.go and b.go must now conflict for m2.
        let conflicts = db.check_conflicts("m2", "/repo", &updated)?;
        assert_eq!(conflicts.len(), 2, "expected 2 per-file conflicts");

        // Calling with an empty file list must release all active claims.
        db.update_file_claims_with_files("m1", "/repo", &[])?;
        let conflicts = db.check_conflicts("m2", "/repo", &updated)?;
        assert_eq!(
            conflicts.len(),
            0,
            "expected 0 conflicts after empty update"
        );
        Ok(())
    }

    /// Proves that a mission whose run ends in failure (no `release_all`
    /// call) keeps its claims active so a parallel mission can still detect
    /// the conflict. Matches Go's `TestLifecycleFailureRetainsClaims`.
    #[test]
    fn lifecycle_failure_retains_claims() -> TestResult {
        let dir = TestDir::new("lifecycle-failure")?;
        let mut db = open_test_db(&dir)?;
        let claimed = files(&["pkg/foo.go", "pkg/bar.go"]);

        // Mission A starts and claims files (simulating first-run start).
        db.claim_files("mission-A", "/repo", &claimed)?;

        // Mission A fails -- no release_all is called. Mission B should
        // still see conflicts.
        let conflicts = db.check_conflicts("mission-B", "/repo", &claimed)?;
        assert_eq!(
            conflicts.len(),
            2,
            "expected 2 conflicts after failure (claims retained)"
        );
        for conflict in &conflicts {
            assert_eq!(conflict.mission_id, "mission-A");
        }
        Ok(())
    }

    /// Proves the resume-to-success path: a previously-failed mission
    /// (claims still active) resumes, updates its per-file claims, succeeds,
    /// and then releases -- leaving no active conflicts. Matches Go's
    /// `TestLifecycleResumeSuccessReleases`.
    #[test]
    fn lifecycle_resume_success_releases() -> TestResult {
        let dir = TestDir::new("lifecycle-resume")?;
        let mut db = open_test_db(&dir)?;

        // First run: claim repo-root marker "." and then fail (no release).
        db.claim_files("mission-A", "/repo", &files(&["."]))?;

        // Resume: update to real per-file claims (mirrors
        // updatePerFileClaimsPostExecution).
        let resume_files = files(&["cmd/main.go", "internal/engine/engine.go"]);
        db.update_file_claims_with_files("mission-A", "/repo", &resume_files)?;

        // Stale "." marker must be gone.
        let conflicts = db.check_conflicts("mission-B", "/repo", &files(&["."]))?;
        assert_eq!(
            conflicts.len(),
            0,
            "expected stale '.' claim released on resume"
        );

        // Per-file claims must be active.
        let conflicts = db.check_conflicts("mission-B", "/repo", &resume_files)?;
        assert_eq!(
            conflicts.len(),
            2,
            "expected 2 active per-file conflicts during resume"
        );

        // Success: release all claims (mirrors the success branch in run.go).
        db.release_all("mission-A")?;

        // No conflicts remain after successful completion.
        let conflicts = db.check_conflicts("mission-B", "/repo", &resume_files)?;
        assert_eq!(
            conflicts.len(),
            0,
            "expected 0 conflicts after successful release"
        );
        Ok(())
    }

    /// Proves that calling `update_file_claims_with_files` multiple times
    /// converges to only the latest file set being active, with no stale
    /// rows leaking through from prior iterations. Matches Go's
    /// `TestLifecycleRepeatedUpdates`.
    #[test]
    fn lifecycle_repeated_updates() -> TestResult {
        let dir = TestDir::new("lifecycle-repeated")?;
        let mut db = open_test_db(&dir)?;

        let sets: Vec<Vec<String>> = vec![
            files(&["a.go", "b.go", "c.go"]),
            files(&["b.go", "d.go"]),
            files(&["e.go"]),
        ];

        for (i, current) in sets.iter().enumerate() {
            db.update_file_claims_with_files("mission-A", "/repo", current)?;

            // Only the current set should conflict; all previous files must
            // be released.
            let conflicts = db.check_conflicts("mission-B", "/repo", current)?;
            assert_eq!(
                conflicts.len(),
                current.len(),
                "iteration {i}: expected {} conflicts",
                current.len()
            );

            // All files from previous iterations must no longer conflict,
            // except for files that also appear in the current set.
            if i > 0 {
                let prev = &sets[i - 1];
                let old = db.check_conflicts("mission-B", "/repo", prev)?;
                let still_active = prev.iter().filter(|f| current.contains(f)).count();
                assert_eq!(
                    old.len(),
                    still_active,
                    "iteration {i}: expected {still_active} overlap conflicts from prev set"
                );
            }
        }

        // Final state: only "e.go" is active.
        let all = files(&["a.go", "b.go", "c.go", "d.go", "e.go"]);
        let conflicts = db.check_conflicts("mission-B", "/repo", &all)?;
        assert_eq!(conflicts.len(), 1, "expected only e.go active at end");
        assert_eq!(conflicts[0].file_path, "e.go");
        Ok(())
    }

    /// Additional coverage (not present in the Go suite) proving
    /// `create_config_dir_matching_go_mkdirall` leaves an already-existing
    /// directory's mode untouched, matching Go's `os.MkdirAll` no-op
    /// semantics on an existing path (`db.go:40`) rather than unconditionally
    /// forcing 0700.
    #[cfg(unix)]
    #[test]
    fn preexisting_config_dir_mode_is_left_untouched() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let dir = TestDir::new("preexisting-dir-mode")?;
        let target = dir.path.join("preexisting");
        std::fs::create_dir_all(&target)?;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;

        create_config_dir_matching_go_mkdirall(&target)?;

        let mode = std::fs::metadata(&target)?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "expected preexisting dir mode left untouched");
        Ok(())
    }

    /// Additional coverage (not present in the Go suite): a directory that
    /// did not exist yet is created at 0700, matching Go's `os.MkdirAll`
    /// behavior on the creation path (`db.go:40`).
    #[cfg(unix)]
    #[test]
    fn newly_created_config_dir_is_private() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let dir = TestDir::new("new-dir-mode")?;
        let target = dir.path.join("brand-new");
        assert!(!target.exists());

        create_config_dir_matching_go_mkdirall(&target)?;

        let mode = std::fs::metadata(&target)?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "expected newly created dir mode 0700");
        Ok(())
    }

    /// Additional coverage (not present in the Go suite) proving
    /// `rfc3339_now_minus` floors the *difference* at full nanosecond
    /// precision rather than pre-truncating `now`/`max_age` to whole
    /// seconds independently, matching Go's
    /// `time.Now().Add(-maxAge).UTC().Format(time.RFC3339)` (`db.go:203`).
    /// Verified by comparing the formatted cutoff for a sub-second
    /// `max_age` against a manually floor-divided nanosecond computation
    /// rather than duplicating the implementation.
    #[test]
    fn rfc3339_now_minus_floors_the_difference_not_the_operands() -> TestResult {
        let max_age = Duration::from_millis(1_500);
        let before = std::time::SystemTime::now();
        let got = rfc3339_now_minus(max_age);
        let after = std::time::SystemTime::now();

        let expected_from = {
            let nanos = before
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| "before is before UNIX_EPOCH")?
                .as_nanos() as i128
                - max_age.as_nanos() as i128;
            format_epoch_secs_rfc3339(i64::try_from(nanos.div_euclid(1_000_000_000))?)
        };
        let expected_to = {
            let nanos = after
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| "after is before UNIX_EPOCH")?
                .as_nanos() as i128
                - max_age.as_nanos() as i128;
            format_epoch_secs_rfc3339(i64::try_from(nanos.div_euclid(1_000_000_000))?)
        };

        assert!(
            got.as_str() >= expected_from.as_str() && got.as_str() <= expected_to.as_str(),
            "expected {got} to fall within [{expected_from}, {expected_to}]"
        );
        Ok(())
    }

    /// Additional coverage (not present in the Go suite) verifying that
    /// `open_claims_db` configures `PRAGMA busy_timeout=5000`, matching Go's
    /// `?_busy_timeout=5000` connection-string parameter (`db.go:46`).
    #[test]
    fn open_sets_busy_timeout_to_5000() -> TestResult {
        let dir = TestDir::new("busy-timeout-pragma")?;
        let db = open_test_db(&dir)?;
        let timeout: i64 = db
            .conn
            .query_row("PRAGMA busy_timeout", (), |row| row.get(0))?;
        assert_eq!(timeout, 5000);
        Ok(())
    }

    /// Additional coverage (not present in the Go suite) proving the
    /// busy_timeout pragma actually causes a second connection's write to
    /// block-and-retry across contention rather than fail immediately,
    /// matching Go's advisory-lock semantics under the shared
    /// `?_busy_timeout=5000` connection string. Uses a background thread
    /// only to release the held lock after a short delay -- the write under
    /// test itself runs on the main test thread, single-threaded from its
    /// own point of view.
    #[test]
    fn busy_timeout_allows_lock_contention_retry() -> TestResult {
        let dir = TestDir::new("busy-timeout-contention")?;
        let db1 = open_test_db(&dir)?;
        let mut db2 = open_test_db(&dir)?;

        // db1 grabs the write lock and holds it on a background thread for a
        // short delay before releasing it.
        db1.conn.execute_batch("BEGIN IMMEDIATE")?;
        let holder = thread::spawn(move || -> rusqlite::Result<()> {
            thread::sleep(std::time::Duration::from_millis(200));
            db1.conn.execute_batch("ROLLBACK")
        });

        let start = std::time::Instant::now();
        db2.claim_files("mission-1", "/repo", &files(&["a.go"]))?;
        let elapsed = start.elapsed();

        holder.join().map_err(|_| "lock-holder thread panicked")??;

        // The write must have actually waited for the lock (proving
        // busy_timeout retried instead of failing instantly) but well within
        // the 5000ms budget (proving it did not exhaust the timeout).
        assert!(
            elapsed >= std::time::Duration::from_millis(100),
            "expected db2's write to wait for contention, elapsed={elapsed:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "expected db2's write to succeed well within the busy_timeout budget, elapsed={elapsed:?}"
        );
        Ok(())
    }
}
