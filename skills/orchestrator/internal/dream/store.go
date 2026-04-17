package dream

import (
	"database/sql"
	"fmt"
	"time"

	_ "modernc.org/sqlite"
)

// Store tracks which transcripts and chunks have been processed so that
// dream runs are incremental and idempotent.
type Store interface {
	// IsFileProcessed returns true if a file with contentHash was already processed.
	IsFileProcessed(contentHash string) (bool, error)
	// MarkFileProcessed records a completed file with its hash, message count, and chunk count.
	MarkFileProcessed(path, contentHash string, msgCount, chunkCount int) error
	// IsChunkProcessed returns true if the chunk with chunkHash was already extracted.
	IsChunkProcessed(chunkHash string) (bool, error)
	// MarkChunkProcessed records a completed chunk.
	MarkChunkProcessed(transcriptPath, chunkHash string, chunkIndex int) error
	// Status returns the total count of processed transcripts and chunks.
	Status() (files, chunks int, err error)
	// Reset clears all processing state (for --force or testing).
	Reset() error
	// Close closes the underlying database connection.
	Close() error
}

// SQLiteStore is the production Store implementation backed by learnings.db.
// It adds two tables (processed_transcripts, processed_chunks) to the existing
// database without touching the learnings schema.
type SQLiteStore struct {
	db *sql.DB
}

// OpenSQLiteStore opens the SQLite database at path, runs the dream table
// migrations, and returns a ready Store. Caller must call Close() when done.
func OpenSQLiteStore(path string) (*SQLiteStore, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, fmt.Errorf("opening dream store %s: %w", path, err)
	}

	if _, err := db.Exec("PRAGMA journal_mode=WAL"); err != nil {
		db.Close()
		return nil, fmt.Errorf("setting WAL mode on dream store: %w", err)
	}

	s := &SQLiteStore{db: db}
	if err := s.migrate(); err != nil {
		db.Close()
		return nil, err
	}
	return s, nil
}

func (s *SQLiteStore) migrate() error {
	// processed_transcripts: one row per JSONL file; keyed by content_hash so
	// unchanged files are skipped without re-parsing.
	_, err := s.db.Exec(`
		CREATE TABLE IF NOT EXISTS processed_transcripts (
			id            INTEGER PRIMARY KEY AUTOINCREMENT,
			path          TEXT    NOT NULL,
			content_hash  TEXT    NOT NULL UNIQUE,
			msg_count     INTEGER NOT NULL DEFAULT 0,
			chunk_count   INTEGER NOT NULL DEFAULT 0,
			processed_at  DATETIME NOT NULL
		)
	`)
	if err != nil {
		return fmt.Errorf("creating processed_transcripts: %w", err)
	}

	// processed_chunks: one row per chunk; keyed by chunk_hash so repeated runs
	// skip extraction without re-calling the LLM.
	_, err = s.db.Exec(`
		CREATE TABLE IF NOT EXISTS processed_chunks (
			id               INTEGER PRIMARY KEY AUTOINCREMENT,
			transcript_path  TEXT    NOT NULL,
			chunk_hash       TEXT    NOT NULL UNIQUE,
			chunk_index      INTEGER NOT NULL DEFAULT 0,
			processed_at     DATETIME NOT NULL
		)
	`)
	if err != nil {
		return fmt.Errorf("creating processed_chunks: %w", err)
	}

	// Indexes for O(1) lookup during the dedup checks.
	s.db.Exec("CREATE INDEX IF NOT EXISTS idx_proc_transcripts_hash ON processed_transcripts(content_hash)")
	s.db.Exec("CREATE INDEX IF NOT EXISTS idx_proc_chunks_hash ON processed_chunks(chunk_hash)")

	return nil
}

func (s *SQLiteStore) IsFileProcessed(contentHash string) (bool, error) {
	var n int
	err := s.db.QueryRow(
		"SELECT COUNT(*) FROM processed_transcripts WHERE content_hash = ?",
		contentHash,
	).Scan(&n)
	if err != nil {
		return false, fmt.Errorf("checking processed transcript: %w", err)
	}
	return n > 0, nil
}

func (s *SQLiteStore) MarkFileProcessed(path, contentHash string, msgCount, chunkCount int) error {
	_, err := s.db.Exec(`
		INSERT OR REPLACE INTO processed_transcripts
			(path, content_hash, msg_count, chunk_count, processed_at)
		VALUES (?, ?, ?, ?, ?)
	`, path, contentHash, msgCount, chunkCount,
		time.Now().UTC().Format(time.RFC3339))
	if err != nil {
		return fmt.Errorf("marking transcript processed: %w", err)
	}
	return nil
}

func (s *SQLiteStore) IsChunkProcessed(chunkHash string) (bool, error) {
	var n int
	err := s.db.QueryRow(
		"SELECT COUNT(*) FROM processed_chunks WHERE chunk_hash = ?",
		chunkHash,
	).Scan(&n)
	if err != nil {
		return false, fmt.Errorf("checking processed chunk: %w", err)
	}
	return n > 0, nil
}

func (s *SQLiteStore) MarkChunkProcessed(transcriptPath, chunkHash string, chunkIndex int) error {
	_, err := s.db.Exec(`
		INSERT OR IGNORE INTO processed_chunks
			(transcript_path, chunk_hash, chunk_index, processed_at)
		VALUES (?, ?, ?, ?)
	`, transcriptPath, chunkHash, chunkIndex,
		time.Now().UTC().Format(time.RFC3339))
	if err != nil {
		return fmt.Errorf("marking chunk processed: %w", err)
	}
	return nil
}

func (s *SQLiteStore) Status() (files, chunks int, err error) {
	if err = s.db.QueryRow("SELECT COUNT(*) FROM processed_transcripts").Scan(&files); err != nil {
		return 0, 0, fmt.Errorf("counting processed transcripts: %w", err)
	}
	if err = s.db.QueryRow("SELECT COUNT(*) FROM processed_chunks").Scan(&chunks); err != nil {
		return 0, 0, fmt.Errorf("counting processed chunks: %w", err)
	}
	return files, chunks, nil
}

func (s *SQLiteStore) Reset() error {
	if _, err := s.db.Exec("DELETE FROM processed_transcripts"); err != nil {
		return fmt.Errorf("clearing processed_transcripts: %w", err)
	}
	if _, err := s.db.Exec("DELETE FROM processed_chunks"); err != nil {
		return fmt.Errorf("clearing processed_chunks: %w", err)
	}
	return nil
}

func (s *SQLiteStore) Close() error {
	return s.db.Close()
}
