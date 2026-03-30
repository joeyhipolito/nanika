package scan

import (
	"context"
	"database/sql"
	"encoding/json"
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

// PersistFindings opens (or creates) findings.db and writes findings using
// INSERT OR IGNORE so duplicate IDs are silently skipped.
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

	now := time.Now().UTC().Format(time.RFC3339)
	for _, f := range findings {
		ev, _ := json.Marshal(f.Evidence)
		foundAt := f.FoundAt.UTC().Format(time.RFC3339)
		if f.FoundAt.IsZero() {
			foundAt = now
		}
		var expiresAt interface{}
		if f.ExpiresAt != nil {
			expiresAt = f.ExpiresAt.UTC().Format(time.RFC3339)
		}
		_, err := db.ExecContext(ctx, `
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
		)
		if err != nil {
			return fmt.Errorf("insert finding %q: %w", f.ID, err)
		}
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
	}
	for _, stmt := range stmts {
		if _, err := db.Exec(stmt); err != nil {
			return fmt.Errorf("exec DDL: %w", err)
		}
	}
	return nil
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
