// Package claims provides an advisory file-claim registry so parallel missions
// targeting the same repository can detect potential edit conflicts.
//
// Claims are warnings, not locks — a mission that finds conflicts will print
// them and continue. The registry lives in a file_claims table added to the
// shared ~/.alluka/learnings.db using the same lazy-init pattern as routing.
package claims

import (
	"database/sql"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	_ "modernc.org/sqlite"
)

// DB wraps the SQLite connection for claim operations.
type DB struct {
	db *sql.DB
}

// Conflict describes a file already claimed by another active mission.
type Conflict struct {
	FilePath  string
	MissionID string
}

// OpenDB opens (or creates) the learnings.db and ensures the file_claims
// table exists. Pass an empty string to use the default ~/.alluka/learnings.db.
func OpenDB(dbPath string) (*DB, error) {
	if dbPath == "" {
		base, err := config.Dir()
		if err != nil {
			return nil, fmt.Errorf("get config dir: %w", err)
		}
		if err := os.MkdirAll(base, 0700); err != nil {
			return nil, fmt.Errorf("create config dir: %w", err)
		}
		dbPath = filepath.Join(base, "learnings.db")
	}

	db, err := sql.Open("sqlite", dbPath+"?_journal_mode=WAL&_busy_timeout=5000")
	if err != nil {
		return nil, fmt.Errorf("open db: %w", err)
	}

	if err := initSchema(db); err != nil {
		db.Close()
		return nil, fmt.Errorf("init schema: %w", err)
	}

	return &DB{db: db}, nil
}

func initSchema(db *sql.DB) error {
	_, err := db.Exec(`
		CREATE TABLE IF NOT EXISTS file_claims (
			file_path   TEXT NOT NULL,
			mission_id  TEXT NOT NULL,
			repo_root   TEXT NOT NULL,
			claimed_at  DATETIME NOT NULL,
			released_at DATETIME,
			PRIMARY KEY (file_path, mission_id)
		)
	`)
	return err
}

// ClaimFiles records advisory claims for all files in the list under missionID.
// Existing claims for the same (file_path, mission_id) pair are replaced so
// that re-runs do not accumulate stale rows.
func (d *DB) ClaimFiles(missionID, repoRoot string, files []string) error {
	if len(files) == 0 {
		return nil
	}

	now := time.Now().UTC().Format(time.RFC3339)

	tx, err := d.db.Begin()
	if err != nil {
		return fmt.Errorf("begin tx: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck

	stmt, err := tx.Prepare(`
		INSERT OR REPLACE INTO file_claims (file_path, mission_id, repo_root, claimed_at, released_at)
		VALUES (?, ?, ?, ?, NULL)
	`)
	if err != nil {
		return fmt.Errorf("prepare stmt: %w", err)
	}
	defer stmt.Close()

	for _, f := range files {
		if _, err := stmt.Exec(f, missionID, repoRoot, now); err != nil {
			return fmt.Errorf("insert claim for %q: %w", f, err)
		}
	}

	return tx.Commit()
}

// CheckConflicts returns active claims on any of the given files by missions
// other than missionID that share the same repoRoot.
func (d *DB) CheckConflicts(missionID, repoRoot string, files []string) ([]Conflict, error) {
	if len(files) == 0 {
		return nil, nil
	}

	placeholders := make([]string, len(files))
	args := make([]any, 0, len(files)+2)
	args = append(args, missionID, repoRoot)
	for i, f := range files {
		placeholders[i] = "?"
		args = append(args, f)
	}

	query := fmt.Sprintf(`
		SELECT file_path, mission_id FROM file_claims
		WHERE mission_id != ?
		  AND repo_root   =  ?
		  AND released_at IS NULL
		  AND file_path IN (%s)
	`, strings.Join(placeholders, ","))

	rows, err := d.db.Query(query, args...)
	if err != nil {
		return nil, fmt.Errorf("query conflicts: %w", err)
	}
	defer rows.Close()

	var conflicts []Conflict
	for rows.Next() {
		var c Conflict
		if err := rows.Scan(&c.FilePath, &c.MissionID); err != nil {
			return nil, fmt.Errorf("scan conflict row: %w", err)
		}
		conflicts = append(conflicts, c)
	}
	return conflicts, rows.Err()
}

// UpdateFileClaimsWithFiles atomically replaces all active claims for
// missionID with per-file claims for the provided files. Any file previously
// claimed but absent from files has its claim released, preventing stale rows
// from prior runs from accumulating. If files is empty the call is equivalent
// to ReleaseAll.
func (d *DB) UpdateFileClaimsWithFiles(missionID, repoRoot string, files []string) error {
	now := time.Now().UTC().Format(time.RFC3339)

	tx, err := d.db.Begin()
	if err != nil {
		return fmt.Errorf("begin tx: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck

	// Release all existing active claims for this mission so that files
	// dropped from the new set are not left with stale active claims.
	if _, err := tx.Exec(`
		UPDATE file_claims SET released_at = ?
		WHERE mission_id = ? AND released_at IS NULL
	`, now, missionID); err != nil {
		return fmt.Errorf("release existing claims: %w", err)
	}

	if len(files) > 0 {
		stmt, err := tx.Prepare(`
			INSERT OR REPLACE INTO file_claims (file_path, mission_id, repo_root, claimed_at, released_at)
			VALUES (?, ?, ?, ?, NULL)
		`)
		if err != nil {
			return fmt.Errorf("prepare insert: %w", err)
		}
		defer stmt.Close()

		for _, f := range files {
			if _, err := stmt.Exec(f, missionID, repoRoot, now); err != nil {
				return fmt.Errorf("insert claim for %q: %w", f, err)
			}
		}
	}

	return tx.Commit()
}

// ReleaseAll marks all active claims for missionID as released.
func (d *DB) ReleaseAll(missionID string) error {
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := d.db.Exec(`
		UPDATE file_claims SET released_at = ?
		WHERE mission_id = ? AND released_at IS NULL
	`, now, missionID)
	return err
}

// PurgeStaleClaims deletes all claim rows (released or not) whose claimed_at
// is older than maxAge. Returns the number of rows deleted.
func (d *DB) PurgeStaleClaims(maxAge time.Duration) (int64, error) {
	cutoff := time.Now().Add(-maxAge).UTC().Format(time.RFC3339)
	res, err := d.db.Exec(`DELETE FROM file_claims WHERE claimed_at < ?`, cutoff)
	if err != nil {
		return 0, fmt.Errorf("purge claims: %w", err)
	}
	return res.RowsAffected()
}

// Close closes the underlying database connection.
func (d *DB) Close() error {
	return d.db.Close()
}
