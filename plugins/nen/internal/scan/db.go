package scan

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"

	_ "modernc.org/sqlite"
)

// MetricsDBPath returns the canonical path to metrics.db.
func MetricsDBPath() (string, error) {
	base, err := Dir()
	if err != nil {
		return "", err
	}
	return filepath.Join(base, "metrics.db"), nil
}

// LearningsDBPath returns the canonical path to learnings.db.
func LearningsDBPath() (string, error) {
	base, err := Dir()
	if err != nil {
		return "", err
	}
	return filepath.Join(base, "learnings.db"), nil
}

// OpenMetricsDB opens metrics.db in WAL read mode with a busy timeout.
// Returns an error (non-fatal to the caller) if the file doesn't exist yet.
func OpenMetricsDB() (*sql.DB, error) {
	path, err := MetricsDBPath()
	if err != nil {
		return nil, fmt.Errorf("metrics db path: %w", err)
	}
	if _, err := os.Stat(path); err != nil {
		return nil, fmt.Errorf("metrics.db not found at %s: %w", path, err)
	}
	db, err := sql.Open("sqlite", path+"?_journal_mode=WAL&_busy_timeout=5000")
	if err != nil {
		return nil, fmt.Errorf("open metrics.db: %w", err)
	}
	return db, nil
}

// OpenLearningsDB opens learnings.db read-only.
// Returns (nil, nil) if the file doesn't exist.
func OpenLearningsDB() (*sql.DB, error) {
	path, err := LearningsDBPath()
	if err != nil {
		return nil, fmt.Errorf("learnings db path: %w", err)
	}
	return OpenReadOnly(path)
}

// FindingsDBPath returns the canonical path to findings.db (~/.alluka/nen/findings.db).
func FindingsDBPath() (string, error) {
	base, err := Dir()
	if err != nil {
		return "", err
	}
	return filepath.Join(base, "nen", "findings.db"), nil
}

// dbExecer is the subset of *sql.DB / *sql.Tx shared by both writers.
type dbExecer interface {
	ExecContext(ctx context.Context, query string, args ...interface{}) (sql.Result, error)
	QueryRowContext(ctx context.Context, query string, args ...interface{}) *sql.Row
}

// UpsertFinding writes a single finding with semantic-key deduplication: if an
// active (not superseded, not expired) row already exists with the same
// (ability, category, scope_kind, scope_value), its severity / title /
// description / evidence / found_at are refreshed in place instead of inserting
// a new row. Otherwise a new row is inserted.
//
// This is the single source of truth for finding writes. Both the direct
// scanner path (PersistFindings) and the daemon ingestion path (nen-daemon's
// event-routed inserts) route through here so dedup behaviour stays consistent.
// Scanners can call this on every scan cycle without growing the table
// unbounded.
func UpsertFinding(ctx context.Context, q dbExecer, f Finding) error {
	ev, _ := json.Marshal(f.Evidence)
	now := time.Now().UTC().Format(time.RFC3339)
	foundAt := f.FoundAt.UTC().Format(time.RFC3339)
	if f.FoundAt.IsZero() {
		foundAt = now
	}
	var expiresAt interface{}
	if f.ExpiresAt != nil {
		expiresAt = f.ExpiresAt.UTC().Format(time.RFC3339)
	}

	// Look for an active finding with the same logical identity.
	var existingID string
	err := q.QueryRowContext(ctx, `
		SELECT id FROM findings
		WHERE ability = ?
		  AND category = ?
		  AND scope_kind = ?
		  AND scope_value = ?
		  AND superseded_by = ''
		  AND (expires_at IS NULL OR datetime(expires_at) > datetime('now'))
		LIMIT 1`,
		f.Ability, f.Category, f.Scope.Kind, f.Scope.Value,
	).Scan(&existingID)

	switch {
	case err == nil:
		// Refresh existing row in place.
		if _, err := q.ExecContext(ctx, `
			UPDATE findings
			SET severity = ?, title = ?, description = ?,
			    evidence = ?, source = ?, found_at = ?, expires_at = ?
			WHERE id = ?`,
			string(f.Severity), f.Title, f.Description,
			string(ev), f.Source, foundAt, expiresAt,
			existingID,
		); err != nil {
			return fmt.Errorf("refresh finding %q: %w", existingID, err)
		}
		return nil
	case errors.Is(err, sql.ErrNoRows):
		// Insert new. INSERT OR IGNORE still guards against same-ID races.
		if _, err := q.ExecContext(ctx, `
			INSERT OR IGNORE INTO findings
				(id, ability, category, severity, title, description,
				 scope_kind, scope_value, evidence, source,
				 found_at, expires_at, superseded_by, created_at)
			VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)`,
			f.ID, f.Ability, f.Category, string(f.Severity),
			f.Title, f.Description,
			f.Scope.Kind, f.Scope.Value,
			string(ev), f.Source,
			foundAt, expiresAt, f.SupersededBy, now,
		); err != nil {
			return fmt.Errorf("insert finding %q: %w", f.ID, err)
		}
		return nil
	default:
		return fmt.Errorf("lookup active finding (%s/%s/%s/%s): %w",
			f.Ability, f.Category, f.Scope.Kind, f.Scope.Value, err)
	}
}

// PersistFindings opens (or creates) findings.db and writes findings with
// semantic-key deduplication via UpsertFinding. Intended for one-shot callers
// (scanners that run and exit). Long-running writers that already hold an open
// DB handle should call UpsertFinding directly.
func PersistFindings(ctx context.Context, findings []Finding) error {
	if len(findings) == 0 {
		return nil
	}
	path, err := FindingsDBPath()
	if err != nil {
		return fmt.Errorf("findings db path: %w", err)
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return fmt.Errorf("create findings db dir: %w", err)
	}
	db, err := sql.Open("sqlite", path+"?_journal_mode=WAL&_busy_timeout=5000")
	if err != nil {
		return fmt.Errorf("open findings.db: %w", err)
	}
	defer db.Close()

	if err := migrateFindings(db); err != nil {
		return fmt.Errorf("migrate findings.db: %w", err)
	}

	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("begin findings tx: %w", err)
	}
	defer func() { _ = tx.Rollback() }()

	for _, f := range findings {
		if err := UpsertFinding(ctx, tx, f); err != nil {
			return err
		}
	}

	if err := tx.Commit(); err != nil {
		return fmt.Errorf("commit findings tx: %w", err)
	}
	return nil
}

func migrateFindings(db *sql.DB) error {
	stmts := []string{
		`CREATE TABLE IF NOT EXISTS findings (
			id            TEXT PRIMARY KEY,
			ability       TEXT NOT NULL,
			category      TEXT NOT NULL,
			severity      TEXT NOT NULL,
			title         TEXT NOT NULL,
			description   TEXT NOT NULL,
			scope_kind    TEXT NOT NULL,
			scope_value   TEXT NOT NULL,
			evidence      TEXT NOT NULL DEFAULT '[]',
			source        TEXT NOT NULL,
			found_at      DATETIME NOT NULL,
			expires_at    DATETIME,
			superseded_by TEXT NOT NULL DEFAULT '',
			created_at    DATETIME NOT NULL
		)`,
		`CREATE INDEX IF NOT EXISTS idx_findings_ability  ON findings(ability)`,
		`CREATE INDEX IF NOT EXISTS idx_findings_severity ON findings(severity)`,
		`CREATE INDEX IF NOT EXISTS idx_findings_found_at ON findings(found_at)`,
		`CREATE INDEX IF NOT EXISTS idx_findings_active   ON findings(superseded_by, expires_at)`,
		// Covers the semantic-key dedup lookup in PersistFindings.
		`CREATE INDEX IF NOT EXISTS idx_findings_identity ON findings(ability, category, scope_kind, scope_value, superseded_by)`,
	}
	for _, stmt := range stmts {
		if _, err := db.Exec(stmt); err != nil {
			return fmt.Errorf("exec DDL: %w", err)
		}
	}
	return nil
}

// SupersedeActiveFindingByScope marks any active finding matching
// (ability, category, scopeKind, scopeValue) as superseded by setting
// superseded_by = supersededBy. No-op if no matching active finding exists.
func SupersedeActiveFindingByScope(ctx context.Context, q dbExecer, ability, category, scopeKind, scopeValue, supersededBy string) error {
	_, err := q.ExecContext(ctx, `
		UPDATE findings
		SET superseded_by = ?
		WHERE ability = ?
		  AND category = ?
		  AND scope_kind = ?
		  AND scope_value = ?
		  AND superseded_by = ''
		  AND (expires_at IS NULL OR datetime(expires_at) > datetime('now'))`,
		supersededBy, ability, category, scopeKind, scopeValue,
	)
	if err != nil {
		return fmt.Errorf("supersede finding (%s/%s/%s/%s): %w", ability, category, scopeKind, scopeValue, err)
	}
	return nil
}

// SupersedeActiveFindingsForScope opens findings.db and supersedes any active
// finding matching the given scope. Returns nil if findings.db does not exist
// yet (nothing to supersede). Intended for one-shot callers.
func SupersedeActiveFindingsForScope(ctx context.Context, ability, category, scopeKind, scopeValue, supersededBy string) error {
	path, err := FindingsDBPath()
	if err != nil {
		return fmt.Errorf("findings db path: %w", err)
	}
	if _, statErr := os.Stat(path); os.IsNotExist(statErr) {
		return nil // no DB yet — nothing to supersede
	}
	db, err := sql.Open("sqlite", path+"?_journal_mode=WAL&_busy_timeout=5000")
	if err != nil {
		return fmt.Errorf("open findings.db: %w", err)
	}
	defer db.Close()

	if err := migrateFindings(db); err != nil {
		return fmt.Errorf("migrate findings.db: %w", err)
	}
	return SupersedeActiveFindingByScope(ctx, db, ability, category, scopeKind, scopeValue, supersededBy)
}

// OpenReadOnly opens a SQLite database file in read-only mode.
// Returns (nil, nil) if the file does not exist.
func OpenReadOnly(path string) (*sql.DB, error) {
	if _, err := os.Stat(path); os.IsNotExist(err) {
		return nil, nil
	} else if err != nil {
		return nil, fmt.Errorf("stat %q: %w", path, err)
	}
	db, err := sql.Open("sqlite", "file:"+path+"?mode=ro")
	if err != nil {
		return nil, fmt.Errorf("open %q read-only: %w", path, err)
	}
	return db, nil
}
