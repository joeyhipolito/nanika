use rusqlite::{Connection, Result, params};
use std::path::Path;

const MIGRATION_V1: &str = include_str!("../migrations/001_initial.sql");
const MIGRATION_V2: &str = include_str!("../migrations/002_parent_id.sql");
const MIGRATION_V3: &str = include_str!("../migrations/003_seq_id.sql");

pub fn open(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    let table_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_migrations'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if table_exists == 0 {
        conn.execute_batch(MIGRATION_V1)?;
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![1, chrono::Utc::now().to_rfc3339()],
        )?;
    }

    let version: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if version < 2 {
        conn.execute_batch(MIGRATION_V2)?;
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![2, chrono::Utc::now().to_rfc3339()],
        )?;
    }

    let version: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if version < 3 {
        conn.execute_batch(MIGRATION_V3)?;
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![3, chrono::Utc::now().to_rfc3339()],
        )?;
    }

    Ok(())
}

pub fn default_db_path() -> std::path::PathBuf {
    let base = std::env::var("TRACKER_DB").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        format!("{}/.alluka/tracker.db", home)
    });
    std::path::PathBuf::from(base)
}

pub fn ensure_dir(db_path: &Path) -> std::io::Result<()> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}
