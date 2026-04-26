package index

import (
	"database/sql"
	"fmt"

	_ "modernc.org/sqlite"
)

// Indexer is a lightweight SQLite-backed note + link-graph index.
// It is distinct from Store: Store handles FTS and embeddings;
// Indexer handles incremental sync and the wikilink graph (TRK-550/551).
type Indexer struct {
	db *sql.DB
}

// OpenIndexer opens or creates the SQLite database at path and runs schema
// migrations on every open (idempotent via IF NOT EXISTS).
func OpenIndexer(path string) (*Indexer, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, fmt.Errorf("open indexer db: %w", err)
	}

	// Single connection: SQLite supports one writer; this avoids "database is
	// locked" errors under concurrent goroutines and keeps pragma state stable.
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	db.SetConnMaxLifetime(0)

	ix := &Indexer{db: db}
	if err := ix.migrate(); err != nil {
		db.Close()
		return nil, err
	}
	return ix, nil
}

// Close releases the database connection.
func (ix *Indexer) Close() error {
	return ix.db.Close()
}

// migrate applies per-connection pragmas and creates the schema if absent.
func (ix *Indexer) migrate() error {
	pragmas := []string{
		"PRAGMA journal_mode=WAL",
		"PRAGMA foreign_keys=ON",
		"PRAGMA busy_timeout=5000",
	}
	for _, p := range pragmas {
		if _, err := ix.db.Exec(p); err != nil {
			return fmt.Errorf("pragma %q: %w", p, err)
		}
	}

	stmts := []string{
		`CREATE TABLE IF NOT EXISTS schema_version (
			version INTEGER NOT NULL
		)`,
		`INSERT INTO schema_version (version)
		 SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM schema_version)`,

		`CREATE TABLE IF NOT EXISTS notes (
			path     TEXT PRIMARY KEY NOT NULL,
			title    TEXT NOT NULL DEFAULT '',
			mod_time INTEGER NOT NULL DEFAULT 0
		)`,

		// src FK → notes(path) ON DELETE CASCADE removes outgoing links when the
		// source note is deleted. dst is intentionally unconstrained: deleting a
		// destination note leaves incoming link records intact (callers may use
		// them to detect dangling references).
		`CREATE TABLE IF NOT EXISTS links (
			src TEXT NOT NULL REFERENCES notes(path) ON DELETE CASCADE,
			dst TEXT NOT NULL,
			PRIMARY KEY (src, dst)
		)`,

		`CREATE INDEX IF NOT EXISTS idx_links_src     ON links(src)`,
		`CREATE INDEX IF NOT EXISTS idx_links_dst     ON links(dst)`,
		`CREATE INDEX IF NOT EXISTS idx_notes_mod_time ON notes(mod_time)`,
	}
	for _, s := range stmts {
		if _, err := ix.db.Exec(s); err != nil {
			return fmt.Errorf("migrate: %w", err)
		}
	}
	return nil
}

// Upsert inserts or updates a note and atomically replaces its outgoing links.
// Old links for path are deleted before the new set is inserted within the
// same transaction, so readers never see a partial link state.
func (ix *Indexer) Upsert(path string, meta NoteMeta, links []string) error {
	tx, err := ix.db.Begin()
	if err != nil {
		return fmt.Errorf("upsert begin tx: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck

	_, err = tx.Exec(
		`INSERT INTO notes (path, title, mod_time) VALUES (?, ?, ?)
		 ON CONFLICT(path) DO UPDATE SET title=excluded.title, mod_time=excluded.mod_time`,
		path, meta.Title, meta.ModTime,
	)
	if err != nil {
		return fmt.Errorf("upsert note %q: %w", path, err)
	}

	if _, err = tx.Exec("DELETE FROM links WHERE src = ?", path); err != nil {
		return fmt.Errorf("clear links for %q: %w", path, err)
	}

	for _, dst := range links {
		if _, err = tx.Exec(
			"INSERT OR IGNORE INTO links (src, dst) VALUES (?, ?)", path, dst,
		); err != nil {
			return fmt.Errorf("insert link %q -> %q: %w", path, dst, err)
		}
	}

	return tx.Commit()
}

// Delete removes a note; ON DELETE CASCADE automatically removes its outgoing
// link rows. Incoming link rows (where path is a dst) are preserved.
func (ix *Indexer) Delete(path string) error {
	if _, err := ix.db.Exec("DELETE FROM notes WHERE path = ?", path); err != nil {
		return fmt.Errorf("delete note %q: %w", path, err)
	}
	return nil
}

// ReplaceLinks atomically swaps the outgoing link set for an existing note.
// The note row itself is not modified. The caller must ensure path exists.
func (ix *Indexer) ReplaceLinks(path string, links []string) error {
	tx, err := ix.db.Begin()
	if err != nil {
		return fmt.Errorf("replace links begin tx: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck

	if _, err = tx.Exec("DELETE FROM links WHERE src = ?", path); err != nil {
		return fmt.Errorf("clear links for %q: %w", path, err)
	}

	for _, dst := range links {
		if _, err = tx.Exec(
			"INSERT OR IGNORE INTO links (src, dst) VALUES (?, ?)", path, dst,
		); err != nil {
			return fmt.Errorf("insert link %q -> %q: %w", path, dst, err)
		}
	}

	return tx.Commit()
}

// GetNote returns the stored metadata for path. Returns (_, false, nil) when
// path is not indexed.
func (ix *Indexer) GetNote(path string) (NoteMeta, bool, error) {
	var m NoteMeta
	err := ix.db.QueryRow(
		"SELECT title, mod_time FROM notes WHERE path = ?", path,
	).Scan(&m.Title, &m.ModTime)
	if err == sql.ErrNoRows {
		return NoteMeta{}, false, nil
	}
	if err != nil {
		return NoteMeta{}, false, fmt.Errorf("get note %q: %w", path, err)
	}
	return m, true, nil
}

// Neighbours returns the outgoing link targets (dst values) for path.
// Returns an empty non-nil slice when path has no outgoing links or is not
// indexed.
func (ix *Indexer) Neighbours(path string) ([]string, error) {
	rows, err := ix.db.Query("SELECT dst FROM links WHERE src = ?", path)
	if err != nil {
		return nil, fmt.Errorf("neighbours %q: %w", path, err)
	}
	defer rows.Close()

	dsts := make([]string, 0)
	for rows.Next() {
		var dst string
		if err := rows.Scan(&dst); err != nil {
			return nil, fmt.Errorf("scan neighbour: %w", err)
		}
		dsts = append(dsts, dst)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("neighbours rows: %w", err)
	}
	return dsts, nil
}

// CountNotes returns the total number of notes stored in the index.
func (ix *Indexer) CountNotes() (int, error) {
	var n int
	if err := ix.db.QueryRow("SELECT COUNT(*) FROM notes").Scan(&n); err != nil {
		return 0, fmt.Errorf("count notes: %w", err)
	}
	return n, nil
}

// CountLinks returns the total number of links stored in the index.
func (ix *Indexer) CountLinks() (int, error) {
	var n int
	if err := ix.db.QueryRow("SELECT COUNT(*) FROM links").Scan(&n); err != nil {
		return 0, fmt.Errorf("count links: %w", err)
	}
	return n, nil
}

// AllNotes returns path → NoteMeta for every note in the index.
// Returns an empty non-nil map when the table is empty.
func (ix *Indexer) AllNotes() (map[string]NoteMeta, error) {
	rows, err := ix.db.Query("SELECT path, title, mod_time FROM notes")
	if err != nil {
		return nil, fmt.Errorf("all notes query: %w", err)
	}
	defer rows.Close()

	notes := make(map[string]NoteMeta)
	for rows.Next() {
		var path string
		var m NoteMeta
		if err := rows.Scan(&path, &m.Title, &m.ModTime); err != nil {
			return nil, fmt.Errorf("scan note: %w", err)
		}
		notes[path] = m
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("all notes rows: %w", err)
	}
	return notes, nil
}

// AllLinks returns every (src, dst) pair in the links table as LinkRows.
// Returns an empty non-nil slice when the table is empty.
func (ix *Indexer) AllLinks() ([]LinkRow, error) {
	rows, err := ix.db.Query("SELECT src, dst FROM links")
	if err != nil {
		return nil, fmt.Errorf("all links query: %w", err)
	}
	defer rows.Close()

	links := make([]LinkRow, 0)
	for rows.Next() {
		var lr LinkRow
		if err := rows.Scan(&lr.Src, &lr.Dst); err != nil {
			return nil, fmt.Errorf("scan link row: %w", err)
		}
		links = append(links, lr)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("all links rows: %w", err)
	}
	return links, nil
}
