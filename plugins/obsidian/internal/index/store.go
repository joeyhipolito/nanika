// Package index provides SQLite-backed full-text and vector search indexing
// for Obsidian vault notes. Uses FTS5 for keyword search and vector embeddings
// stored as blobs for semantic search.
package index

import (
	"database/sql"
	"encoding/binary"
	"fmt"
	"math"
	"strings"

	_ "modernc.org/sqlite"
)

// EmbeddingDimensions is the size of Gemini text-embedding-004 vectors.
const EmbeddingDimensions = 768

// Store manages the SQLite search index for an Obsidian vault.
type Store struct {
	db *sql.DB
}

// NoteRow represents a row in the notes table.
type NoteRow struct {
	Path      string
	Title     string
	Tags      string // comma-separated
	Headings  string // newline-separated
	Wikilinks string // comma-separated
	Body      string
	ModTime   int64
	Embedding []float32
}

// Open opens or creates the SQLite index database at the given path.
// Creates the schema if it doesn't exist.
func Open(dbPath string) (*Store, error) {
	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		return nil, fmt.Errorf("failed to open index database: %w", err)
	}

	// Enable WAL mode for better concurrent read performance
	if _, err := db.Exec("PRAGMA journal_mode=WAL"); err != nil {
		db.Close()
		return nil, fmt.Errorf("failed to set WAL mode: %w", err)
	}

	s := &Store{db: db}
	if err := s.createSchema(); err != nil {
		db.Close()
		return nil, err
	}

	return s, nil
}

// Close closes the database connection.
func (s *Store) Close() error {
	return s.db.Close()
}

// createSchema creates the tables if they don't exist.
func (s *Store) createSchema() error {
	// Main notes table with metadata and vector embedding blob
	_, err := s.db.Exec(`
		CREATE TABLE IF NOT EXISTS notes (
			path      TEXT PRIMARY KEY,
			title     TEXT NOT NULL DEFAULT '',
			tags      TEXT NOT NULL DEFAULT '',
			headings  TEXT NOT NULL DEFAULT '',
			wikilinks TEXT NOT NULL DEFAULT '',
			body      TEXT NOT NULL DEFAULT '',
			mod_time  INTEGER NOT NULL DEFAULT 0,
			embedding BLOB
		)
	`)
	if err != nil {
		return fmt.Errorf("failed to create notes table: %w", err)
	}

	// FTS5 virtual table for keyword search over title, tags, headings, body
	_, err = s.db.Exec(`
		CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
			path,
			title,
			tags,
			headings,
			body,
			content='notes',
			content_rowid='rowid'
		)
	`)
	if err != nil {
		return fmt.Errorf("failed to create FTS5 table: %w", err)
	}

	// Triggers to keep FTS5 in sync with the notes table
	triggers := []string{
		`CREATE TRIGGER IF NOT EXISTS notes_ai AFTER INSERT ON notes BEGIN
			INSERT INTO notes_fts(rowid, path, title, tags, headings, body)
			VALUES (new.rowid, new.path, new.title, new.tags, new.headings, new.body);
		END`,
		`CREATE TRIGGER IF NOT EXISTS notes_ad AFTER DELETE ON notes BEGIN
			INSERT INTO notes_fts(notes_fts, rowid, path, title, tags, headings, body)
			VALUES ('delete', old.rowid, old.path, old.title, old.tags, old.headings, old.body);
		END`,
		`CREATE TRIGGER IF NOT EXISTS notes_au AFTER UPDATE ON notes BEGIN
			INSERT INTO notes_fts(notes_fts, rowid, path, title, tags, headings, body)
			VALUES ('delete', old.rowid, old.path, old.title, old.tags, old.headings, old.body);
			INSERT INTO notes_fts(rowid, path, title, tags, headings, body)
			VALUES (new.rowid, new.path, new.title, new.tags, new.headings, new.body);
		END`,
	}
	for _, t := range triggers {
		if _, err := s.db.Exec(t); err != nil {
			return fmt.Errorf("failed to create trigger: %w", err)
		}
	}

	if err := s.createClustersTable(); err != nil {
		return err
	}

	if err := s.createCanonicalNotesTable(); err != nil {
		return err
	}

	return nil
}

// GetModTime returns the stored mod_time for a note path, or 0 if not indexed.
func (s *Store) GetModTime(path string) (int64, error) {
	var modTime int64
	err := s.db.QueryRow("SELECT mod_time FROM notes WHERE path = ?", path).Scan(&modTime)
	if err == sql.ErrNoRows {
		return 0, nil
	}
	return modTime, err
}

// GetAllPaths returns all indexed note paths.
func (s *Store) GetAllPaths() (map[string]bool, error) {
	rows, err := s.db.Query("SELECT path FROM notes")
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	paths := make(map[string]bool)
	for rows.Next() {
		var path string
		if err := rows.Scan(&path); err != nil {
			return nil, err
		}
		paths[path] = true
	}
	return paths, rows.Err()
}

// UpsertNote inserts or updates a note in the index.
func (s *Store) UpsertNote(note *NoteRow) error {
	var embBlob []byte
	if note.Embedding != nil {
		embBlob = encodeEmbedding(note.Embedding)
	}

	_, err := s.db.Exec(`
		INSERT INTO notes (path, title, tags, headings, wikilinks, body, mod_time, embedding)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(path) DO UPDATE SET
			title     = excluded.title,
			tags      = excluded.tags,
			headings  = excluded.headings,
			wikilinks = excluded.wikilinks,
			body      = excluded.body,
			mod_time  = excluded.mod_time,
			embedding = excluded.embedding
	`, note.Path, note.Title, note.Tags, note.Headings, note.Wikilinks, note.Body, note.ModTime, embBlob)
	return err
}

// DeleteNote removes a note from the index.
func (s *Store) DeleteNote(path string) error {
	_, err := s.db.Exec("DELETE FROM notes WHERE path = ?", path)
	return err
}

// NoteCount returns the total number of indexed notes.
func (s *Store) NoteCount() (int, error) {
	var count int
	err := s.db.QueryRow("SELECT COUNT(*) FROM notes").Scan(&count)
	return count, err
}

// SearchResult holds a single search match.
type SearchResult struct {
	Path    string  `json:"path"`
	Title   string  `json:"title"`
	Score   float64 `json:"score"`
	Snippet string  `json:"snippet"`
}

// SearchKeyword performs an FTS5 keyword search.
func (s *Store) SearchKeyword(query string, limit int) ([]SearchResult, error) {
	rows, err := s.db.Query(`
		SELECT n.path, n.title, rank, snippet(notes_fts, 4, '»', '«', '…', 32)
		FROM notes_fts
		JOIN notes n ON notes_fts.path = n.path
		WHERE notes_fts MATCH ?
		ORDER BY rank
		LIMIT ?
	`, query, limit)
	if err != nil {
		return nil, fmt.Errorf("FTS5 search failed: %w", err)
	}
	defer rows.Close()

	var results []SearchResult
	for rows.Next() {
		var r SearchResult
		if err := rows.Scan(&r.Path, &r.Title, &r.Score, &r.Snippet); err != nil {
			return nil, err
		}
		// FTS5 rank is negative (lower = better), normalize to 0-1 range
		r.Score = -r.Score
		results = append(results, r)
	}
	return results, rows.Err()
}

// SearchSemantic performs vector similarity search using cosine similarity.
func (s *Store) SearchSemantic(queryEmbedding []float32, limit int) ([]SearchResult, error) {
	rows, err := s.db.Query("SELECT path, title, embedding FROM notes WHERE embedding IS NOT NULL")
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var results []SearchResult
	for rows.Next() {
		var path, title string
		var embBlob []byte
		if err := rows.Scan(&path, &title, &embBlob); err != nil {
			return nil, err
		}

		emb := decodeEmbedding(embBlob)
		if emb == nil {
			continue
		}

		score := CosineSimilarity(queryEmbedding, emb)
		if score > 0 {
			results = append(results, SearchResult{
				Path:  path,
				Title: title,
				Score: float64(score),
			})
		}
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}

	// Sort by score descending
	sortResults(results)

	if len(results) > limit {
		results = results[:limit]
	}
	return results, nil
}

// SearchHybrid combines FTS5 keyword and semantic vector search with RRF ranking.
func (s *Store) SearchHybrid(query string, queryEmbedding []float32, limit int) ([]SearchResult, error) {
	// Get both result sets
	keywordResults, err := s.SearchKeyword(query, limit*2)
	if err != nil {
		return nil, err
	}

	semanticResults, err := s.SearchSemantic(queryEmbedding, limit*2)
	if err != nil {
		return nil, err
	}

	// Reciprocal Rank Fusion (RRF) with k=60
	const k = 60.0
	scores := make(map[string]float64)
	titles := make(map[string]string)
	snippets := make(map[string]string)

	for i, r := range keywordResults {
		scores[r.Path] += 1.0 / (k + float64(i+1))
		titles[r.Path] = r.Title
		snippets[r.Path] = r.Snippet
	}
	for i, r := range semanticResults {
		scores[r.Path] += 1.0 / (k + float64(i+1))
		if titles[r.Path] == "" {
			titles[r.Path] = r.Title
		}
	}

	// Build combined results
	var results []SearchResult
	for path, score := range scores {
		results = append(results, SearchResult{
			Path:    path,
			Title:   titles[path],
			Score:   score,
			Snippet: snippets[path],
		})
	}

	sortResults(results)

	if len(results) > limit {
		results = results[:limit]
	}
	return results, nil
}

// sortResults sorts search results by score descending.
func sortResults(results []SearchResult) {
	// Simple insertion sort — result sets are small
	for i := 1; i < len(results); i++ {
		for j := i; j > 0 && results[j].Score > results[j-1].Score; j-- {
			results[j], results[j-1] = results[j-1], results[j]
		}
	}
}

// encodeEmbedding converts a float32 slice to a byte slice (little-endian).
func encodeEmbedding(v []float32) []byte {
	buf := make([]byte, len(v)*4)
	for i, f := range v {
		binary.LittleEndian.PutUint32(buf[i*4:], math.Float32bits(f))
	}
	return buf
}

// decodeEmbedding converts a byte slice back to a float32 slice.
func decodeEmbedding(b []byte) []float32 {
	if len(b) == 0 || len(b)%4 != 0 {
		return nil
	}
	v := make([]float32, len(b)/4)
	for i := range v {
		v[i] = math.Float32frombits(binary.LittleEndian.Uint32(b[i*4:]))
	}
	return v
}

// CosineSimilarity calculates the cosine similarity between two vectors.
// Returns a value between -1 and 1, where 1 means identical.
func CosineSimilarity(a, b []float32) float32 {
	if len(a) != len(b) || len(a) == 0 {
		return 0
	}

	var dotProduct, normA, normB float32
	for i := range a {
		dotProduct += a[i] * b[i]
		normA += a[i] * a[i]
		normB += b[i] * b[i]
	}

	if normA == 0 || normB == 0 {
		return 0
	}

	return dotProduct / (sqrt32(normA) * sqrt32(normB))
}

// sqrt32 computes float32 square root using Newton's method.
func sqrt32(x float32) float32 {
	if x <= 0 {
		return 0
	}
	z := x / 2
	for i := 0; i < 10; i++ {
		z = (z + x/z) / 2
	}
	return z
}

// GetAllNoteRows returns all indexed notes with their metadata and embeddings.
func (s *Store) GetAllNoteRows() ([]NoteRow, error) {
	rows, err := s.db.Query("SELECT path, title, tags, headings, wikilinks, body, mod_time, embedding FROM notes")
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var notes []NoteRow
	for rows.Next() {
		var n NoteRow
		var embBlob []byte
		if err := rows.Scan(&n.Path, &n.Title, &n.Tags, &n.Headings, &n.Wikilinks, &n.Body, &n.ModTime, &embBlob); err != nil {
			return nil, err
		}
		n.Embedding = decodeEmbedding(embBlob)
		notes = append(notes, n)
	}
	return notes, rows.Err()
}

// EmbeddingCount returns the number of notes that have embeddings.
func (s *Store) EmbeddingCount() (int, error) {
	var count int
	err := s.db.QueryRow("SELECT COUNT(*) FROM notes WHERE embedding IS NOT NULL").Scan(&count)
	return count, err
}

// IndexDBPath returns the path to the index database for a given vault.
func IndexDBPath(vaultPath string) string {
	return vaultPath + "/.obsidian/search.db"
}

// ClusterRow represents a row in the clusters table.
type ClusterRow struct {
	ClusterID     string
	MemberPaths   string // JSON-encoded []string
	AvgSimilarity float64
	Size          int
	DetectedAt    int64
}

// createClustersTable creates the clusters table if it doesn't exist.
// Called from createSchema so existing DBs pick it up on next open.
func (s *Store) createClustersTable() error {
	_, err := s.db.Exec(`
		CREATE TABLE IF NOT EXISTS clusters (
			cluster_id     TEXT PRIMARY KEY,
			member_paths   TEXT NOT NULL,
			avg_similarity REAL NOT NULL,
			size           INTEGER NOT NULL,
			detected_at    INTEGER NOT NULL
		)
	`)
	if err != nil {
		return fmt.Errorf("failed to create clusters table: %w", err)
	}
	return nil
}

// UpsertCluster inserts or replaces a cluster row.
func (s *Store) UpsertCluster(r ClusterRow) error {
	_, err := s.db.Exec(`
		INSERT INTO clusters (cluster_id, member_paths, avg_similarity, size, detected_at)
		VALUES (?, ?, ?, ?, ?)
		ON CONFLICT(cluster_id) DO UPDATE SET
			member_paths   = excluded.member_paths,
			avg_similarity = excluded.avg_similarity,
			size           = excluded.size,
			detected_at    = excluded.detected_at
	`, r.ClusterID, r.MemberPaths, r.AvgSimilarity, r.Size, r.DetectedAt)
	return err
}

// DeleteAllClusters removes all rows from the clusters table.
func (s *Store) DeleteAllClusters() error {
	_, err := s.db.Exec("DELETE FROM clusters")
	return err
}

// GetAllClusters returns all stored cluster rows.
func (s *Store) GetAllClusters() ([]ClusterRow, error) {
	rows, err := s.db.Query("SELECT cluster_id, member_paths, avg_similarity, size, detected_at FROM clusters ORDER BY avg_similarity DESC")
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var clusters []ClusterRow
	for rows.Next() {
		var r ClusterRow
		if err := rows.Scan(&r.ClusterID, &r.MemberPaths, &r.AvgSimilarity, &r.Size, &r.DetectedAt); err != nil {
			return nil, err
		}
		clusters = append(clusters, r)
	}
	return clusters, rows.Err()
}

// CanonicalRow represents a row in the canonical_notes table.
type CanonicalRow struct {
	ClusterID     string
	CanonicalPath string
	CreatedAt     int64
}

// createCanonicalNotesTable creates the canonical_notes table if it doesn't exist.
func (s *Store) createCanonicalNotesTable() error {
	_, err := s.db.Exec(`
		CREATE TABLE IF NOT EXISTS canonical_notes (
			cluster_id     TEXT PRIMARY KEY,
			canonical_path TEXT NOT NULL,
			created_at     INTEGER NOT NULL
		)
	`)
	if err != nil {
		return fmt.Errorf("failed to create canonical_notes table: %w", err)
	}
	return nil
}

// UpsertCanonical inserts or replaces a canonical note record.
func (s *Store) UpsertCanonical(r CanonicalRow) error {
	_, err := s.db.Exec(`
		INSERT INTO canonical_notes (cluster_id, canonical_path, created_at)
		VALUES (?, ?, ?)
		ON CONFLICT(cluster_id) DO UPDATE SET
			canonical_path = excluded.canonical_path,
			created_at     = excluded.created_at
	`, r.ClusterID, r.CanonicalPath, r.CreatedAt)
	return err
}

// GetCanonicalByClusterID returns the canonical note record for a cluster.
// Returns false if not found.
func (s *Store) GetCanonicalByClusterID(clusterID string) (CanonicalRow, bool, error) {
	var r CanonicalRow
	err := s.db.QueryRow(
		"SELECT cluster_id, canonical_path, created_at FROM canonical_notes WHERE cluster_id = ?",
		clusterID,
	).Scan(&r.ClusterID, &r.CanonicalPath, &r.CreatedAt)
	if err == sql.ErrNoRows {
		return CanonicalRow{}, false, nil
	}
	if err != nil {
		return CanonicalRow{}, false, err
	}
	return r, true, nil
}

// GetAllCanonicals returns all canonical note records.
func (s *Store) GetAllCanonicals() ([]CanonicalRow, error) {
	rows, err := s.db.Query("SELECT cluster_id, canonical_path, created_at FROM canonical_notes")
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var result []CanonicalRow
	for rows.Next() {
		var r CanonicalRow
		if err := rows.Scan(&r.ClusterID, &r.CanonicalPath, &r.CreatedAt); err != nil {
			return nil, err
		}
		result = append(result, r)
	}
	return result, rows.Err()
}

// GetModTimes returns mod_time for each of the given paths.
// Paths not found in the index are omitted from the result.
func (s *Store) GetModTimes(paths []string) (map[string]int64, error) {
	if len(paths) == 0 {
		return map[string]int64{}, nil
	}

	placeholders := strings.Repeat("?,", len(paths))
	placeholders = placeholders[:len(placeholders)-1]

	args := make([]any, len(paths))
	for i, p := range paths {
		args[i] = p
	}

	rows, err := s.db.Query(
		"SELECT path, mod_time FROM notes WHERE path IN ("+placeholders+")",
		args...,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	result := make(map[string]int64, len(paths))
	for rows.Next() {
		var path string
		var modTime int64
		if err := rows.Scan(&path, &modTime); err != nil {
			return nil, err
		}
		result[path] = modTime
	}
	return result, rows.Err()
}

// RandomOldNotes returns up to limit randomly selected notes with mod_time < olderThan.
func (s *Store) RandomOldNotes(olderThan int64, limit int) ([]NoteRow, error) {
	rows, err := s.db.Query(
		"SELECT path, title, body, mod_time FROM notes WHERE mod_time < ? AND mod_time > 0 ORDER BY RANDOM() LIMIT ?",
		olderThan, limit,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var notes []NoteRow
	for rows.Next() {
		var n NoteRow
		if err := rows.Scan(&n.Path, &n.Title, &n.Body, &n.ModTime); err != nil {
			return nil, err
		}
		notes = append(notes, n)
	}
	return notes, rows.Err()
}

// BuildSearchText creates a combined text for embedding from note fields.
func BuildSearchText(title, tags, headings, body string) string {
	var parts []string
	if title != "" {
		parts = append(parts, title)
	}
	if tags != "" {
		parts = append(parts, tags)
	}
	if headings != "" {
		parts = append(parts, headings)
	}
	if body != "" {
		// Truncate body to ~8000 chars for embedding API limits
		if len(body) > 8000 {
			body = body[:8000]
		}
		parts = append(parts, body)
	}
	return strings.Join(parts, "\n")
}
