package scan

import (
	"database/sql"
	"fmt"
	"os"
	"path/filepath"

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
