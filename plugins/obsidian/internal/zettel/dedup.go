// dedup.go — RFC §7 (TRK-525 Phase 1B): SQLite-backed deduplication for mission writes.
package zettel

import (
	"database/sql"
	"fmt"
	"sync"

	_ "modernc.org/sqlite"
)

// DedupDB tracks written mission IDs to prevent duplicate writes.
type DedupDB struct {
	db *sql.DB
	mu sync.Mutex
}

// OpenDedupDB opens or creates the dedup database.
func OpenDedupDB(cachePath string) (*DedupDB, error) {
	db, err := sql.Open("sqlite", cachePath)
	if err != nil {
		return nil, fmt.Errorf("cannot open dedup db: %w", err)
	}

	// Create schema if needed
	schema := `
	CREATE TABLE IF NOT EXISTS written (
		mission_id TEXT PRIMARY KEY,
		path TEXT NOT NULL,
		written_at INTEGER NOT NULL
	);
	`
	if _, err := db.Exec(schema); err != nil {
		return nil, fmt.Errorf("cannot create schema: %w", err)
	}

	return &DedupDB{db: db}, nil
}

// HasMission checks if a mission has already been written.
// Returns (exists, path if exists, error).
func (d *DedupDB) HasMission(id string) (bool, string, error) {
	d.mu.Lock()
	defer d.mu.Unlock()

	var path string
	err := d.db.QueryRow("SELECT path FROM written WHERE mission_id = ?", id).Scan(&path)
	if err == sql.ErrNoRows {
		return false, "", nil
	}
	if err != nil {
		return false, "", fmt.Errorf("cannot query mission: %w", err)
	}
	return true, path, nil
}

// RecordWrite records a written mission.
func (d *DedupDB) RecordWrite(id, path string) error {
	d.mu.Lock()
	defer d.mu.Unlock()

	_, err := d.db.Exec(
		"INSERT INTO written (mission_id, path, written_at) VALUES (?, ?, ?)",
		id, path, int64(0), // timestamp; not used for now
	)
	if err != nil {
		return fmt.Errorf("cannot record write: %w", err)
	}
	return nil
}

// Close closes the database connection.
func (d *DedupDB) Close() error {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.db != nil {
		return d.db.Close()
	}
	return nil
}
