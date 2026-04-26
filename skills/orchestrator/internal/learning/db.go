package learning

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	_ "modernc.org/sqlite"
)

// ErrEmbedderRequired is returned by Insert when an embedder is configured but
// fails to produce a non-empty embedding vector for the supplied learning.
var ErrEmbedderRequired = errors.New("learning: embedder required")

// minRelevanceScore is the minimum combined relevance score for a learning
// to be included in FindRelevant results. Applied to both per-embedding
// similarity checks during scoring and to the final sorted candidates.
const minRelevanceScore = 0.25

// maxSupportedVersion is the highest schema_version this binary understands.
// Increment this constant alongside any future schema migration that requires
// a new binary to read the database.
const maxSupportedVersion = 1

// mmrLambda trades relevance vs. diversity in FindTopByQuality's MMR re-rank.
// Higher values favor relevance; lower values favor diversity.
const mmrLambda = 0.7

// maxPerType caps how many learnings of the same LearningType may appear in
// FindTopByQuality results. Prevents any single type from dominating cold-start
// injection — see TRK-512.
const maxPerType = 5

// DB wraps a SQLite database for learnings storage.
type DB struct {
	db *sql.DB
}

// OpenDB opens or creates the learnings database.
func OpenDB(path string) (*DB, error) {
	if path == "" {
		base, err := config.Dir()
		if err != nil {
			return nil, fmt.Errorf("config dir: %w", err)
		}
		path = filepath.Join(base, "learnings.db")
	}

	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return nil, err
	}

	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, err
	}

	if _, err := db.Exec("PRAGMA journal_mode=WAL"); err != nil {
		db.Close()
		return nil, err
	}

	ldb := &DB{db: db}
	if err := ldb.initSchema(); err != nil {
		db.Close()
		return nil, err
	}

	return ldb, nil
}

// Close closes the database.
func (d *DB) Close() error {
	return d.db.Close()
}

func (d *DB) initSchema() error {
	// Bootstrap schema_version table and seed version 1 on first open.
	if _, err := d.db.Exec(`
		CREATE TABLE IF NOT EXISTS schema_version (
			version INTEGER NOT NULL
		)
	`); err != nil {
		return fmt.Errorf("creating schema_version table: %w", err)
	}
	if _, err := d.db.Exec(
		`INSERT INTO schema_version (version) SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM schema_version)`,
	); err != nil {
		return fmt.Errorf("seeding schema_version: %w", err)
	}
	var version int
	if err := d.db.QueryRow(`SELECT version FROM schema_version`).Scan(&version); err != nil {
		return fmt.Errorf("reading schema version: %w", err)
	}
	if version > maxSupportedVersion {
		return fmt.Errorf("database schema version %d exceeds max supported version %d; upgrade the orchestrator binary", version, maxSupportedVersion)
	}

	_, err := d.db.Exec(`
		CREATE TABLE IF NOT EXISTS learnings (
			id TEXT PRIMARY KEY,
			type TEXT NOT NULL,
			content TEXT NOT NULL,
			context TEXT DEFAULT '',
			domain TEXT NOT NULL,
			worker_name TEXT DEFAULT '',
			workspace_id TEXT DEFAULT '',
			tags TEXT DEFAULT '',
			seen_count INTEGER DEFAULT 1,
			used_count INTEGER DEFAULT 0,
			quality_score REAL DEFAULT 0.0,
			created_at DATETIME NOT NULL,
			last_used_at DATETIME,
			embedding BLOB
		)
	`)
	if err != nil {
		return err
	}

	_, err = d.db.Exec(`
		CREATE VIRTUAL TABLE IF NOT EXISTS learnings_fts USING fts5(
			content, context, domain,
			content='learnings', content_rowid='rowid'
		)
	`)
	if err != nil {
		return err
	}

	triggers := []string{
		`CREATE TRIGGER IF NOT EXISTS learnings_ai AFTER INSERT ON learnings BEGIN
			INSERT INTO learnings_fts(rowid, content, context, domain)
			VALUES (new.rowid, new.content, new.context, new.domain);
		END`,
		`CREATE TRIGGER IF NOT EXISTS learnings_ad AFTER DELETE ON learnings BEGIN
			INSERT INTO learnings_fts(learnings_fts, rowid, content, context, domain)
			VALUES ('delete', old.rowid, old.content, old.context, old.domain);
		END`,
		`CREATE TRIGGER IF NOT EXISTS learnings_au AFTER UPDATE ON learnings BEGIN
			INSERT INTO learnings_fts(learnings_fts, rowid, content, context, domain)
			VALUES ('delete', old.rowid, old.content, old.context, old.domain);
			INSERT INTO learnings_fts(rowid, content, context, domain)
			VALUES (new.rowid, new.content, new.context, new.domain);
		END`,
	}
	for _, t := range triggers {
		if _, err := d.db.Exec(t); err != nil {
			return err
		}
	}

	d.db.Exec("CREATE INDEX IF NOT EXISTS idx_learnings_domain ON learnings(domain)")
	d.db.Exec("CREATE INDEX IF NOT EXISTS idx_learnings_type ON learnings(type)")

	// Migrations: add columns that may not exist in older schemas.
	// archived must be added before idx_learnings_domain_archived is created.
	migrations := []string{
		"ALTER TABLE learnings ADD COLUMN worker_name TEXT DEFAULT ''",
		"ALTER TABLE learnings ADD COLUMN workspace_id TEXT DEFAULT ''",
		"ALTER TABLE learnings ADD COLUMN injection_count INTEGER DEFAULT 0",
		"ALTER TABLE learnings ADD COLUMN compliance_count INTEGER DEFAULT 0",
		"ALTER TABLE learnings ADD COLUMN compliance_rate REAL DEFAULT 0.0",
		"ALTER TABLE learnings ADD COLUMN archived INTEGER DEFAULT 0",
		"ALTER TABLE learnings ADD COLUMN promoted_at DATETIME",
	}
	for _, m := range migrations {
		d.db.Exec(m) // ignore errors (column already exists)
	}

	// Composite index on (domain, archived) — created after migrations to
	// guarantee the archived column exists on both fresh and upgraded DBs.
	d.db.Exec("CREATE INDEX IF NOT EXISTS idx_learnings_domain_archived ON learnings(domain, archived)")

	return nil
}

// Insert stores a learning with dedup.
//
// Write-path guard: when an embedder is supplied, the resulting row must have
// a non-nil embedding before it lands in the table. If embedding generation
// fails or the call returns an empty vector, Insert returns ErrEmbedderRequired
// (wrapped) and writes nothing — this prevents the silent NULL-embedding rows
// that the backfill subcommand exists to repair. Callers without an embedder
// (e.g. docs ingestion without a configured API key) may pass embedder = nil;
// that legacy path is preserved and stores a row with NULL embedding.
func (d *DB) Insert(ctx context.Context, l Learning, embedder *Embedder) error {
	// Generate embedding if available
	if embedder != nil && l.Embedding == nil {
		emb, err := embedder.Embed(ctx, l.Content)
		if err != nil {
			return fmt.Errorf("learning: embedding failed for %s (%s): %w", l.ID, l.Type, errors.Join(ErrEmbedderRequired, err))
		}
		l.Embedding = emb
	}
	if embedder != nil && len(l.Embedding) == 0 {
		return fmt.Errorf("learning: refusing to insert %s (%s) with nil embedding (embedder configured): %w", l.ID, l.Type, ErrEmbedderRequired)
	}

	// Dedup: check cosine similarity against existing
	if l.Embedding != nil {
		matchID, sim := d.findMostSimilar(l.Domain, l.Content, l.Embedding)
		if matchID != "" && sim > 0.85 {
			d.db.Exec("UPDATE learnings SET seen_count = seen_count + 1 WHERE id = ?", matchID)
			return nil // duplicate
		}
	}

	var embBlob []byte
	if l.Embedding != nil {
		embBlob = EncodeEmbedding(l.Embedding)
	}

	tags := strings.Join(l.Tags, ",")

	res, err := d.db.Exec(`
		INSERT OR IGNORE INTO learnings (
			id, type, content, context, domain, worker_name, workspace_id,
			tags, seen_count, used_count, quality_score, created_at, last_used_at, embedding
		) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
	`,
		l.ID, string(l.Type), l.Content, l.Context, l.Domain,
		l.WorkerName, l.WorkspaceID,
		tags, max(l.SeenCount, 1), l.UsageCount, l.QualityScore,
		l.CreatedAt.UTC().Format(time.RFC3339),
		formatNullableTime(l.LastUsedAt),
		embBlob,
	)
	if err != nil {
		return err
	}

	if n, _ := res.RowsAffected(); n > 0 {
		getEmitter().Emit(ctx, event.New(
			event.LearningStored,
			l.WorkspaceID, "", l.WorkerName,
			map[string]any{
				"learning_id":   l.ID,
				"learning_type": string(l.Type),
				"content":       l.Content,
				"worker_name":   l.WorkerName,
				"domain":        l.Domain,
			},
		))
	}
	return nil
}

// findMostSimilar returns the ID and cosine similarity of the existing learning
// most similar to queryEmb. Uses union strategy: FTS top 50 by text relevance
// plus a full domain scan — merged and deduplicated — so semantically similar
// but lexically different entries are always considered.
func (d *DB) findMostSimilar(domain, content string, queryEmb []float32) (string, float64) {
	type candidate struct {
		id      string
		embBlob []byte
	}

	seen := make(map[string]bool)
	var candidates []candidate

	// FTS top 50 by text relevance.
	if content != "" {
		ftsRows, err := d.db.Query(`
			SELECT l.id, l.embedding
			FROM learnings_fts fts
			JOIN learnings l ON fts.rowid = l.rowid
			WHERE learnings_fts MATCH ? AND l.domain = ? AND l.archived = 0
				AND l.embedding IS NOT NULL
			ORDER BY rank
			LIMIT 50
		`, content, domain)
		if err == nil {
			for ftsRows.Next() {
				var c candidate
				if ftsRows.Scan(&c.id, &c.embBlob) == nil {
					seen[c.id] = true
					candidates = append(candidates, c)
				}
			}
			ftsRows.Close()
		}
	}

	// Full domain scan — independent of FTS — so semantically similar but
	// lexically different entries are never missed.
	domRows, err := d.db.Query(
		"SELECT id, embedding FROM learnings WHERE domain = ? AND embedding IS NOT NULL AND archived = 0",
		domain,
	)
	if err == nil {
		for domRows.Next() {
			var c candidate
			if domRows.Scan(&c.id, &c.embBlob) == nil && !seen[c.id] {
				seen[c.id] = true
				candidates = append(candidates, c)
			}
		}
		domRows.Close()
	}

	var bestID string
	var bestSim float64
	for _, c := range candidates {
		emb := DecodeEmbedding(c.embBlob)
		if emb == nil {
			continue
		}
		sim := CosineSimilarity(queryEmb, emb)
		if sim > bestSim {
			bestSim = sim
			bestID = c.id
		}
	}
	return bestID, bestSim
}

// FindRelevant retrieves relevant learnings using hybrid FTS5 + semantic search.
// Optional focusAreas boost results aligned with persona learning interests.
func (d *DB) FindRelevant(ctx context.Context, query, domain string, limit int, embedder *Embedder, focusAreas ...[]string) ([]Learning, error) {
	var queryEmb []float32
	if embedder != nil {
		emb, err := embedder.Embed(ctx, query)
		if err == nil {
			queryEmb = emb
		}
	}

	var fa []string
	if len(focusAreas) > 0 {
		fa = focusAreas[0]
	}

	return d.hybridSearch(domain, query, queryEmb, limit, fa)
}

// hybridSearch retrieves relevant learnings using a union strategy: FTS and
// embedding search run independently, their candidate sets are merged and
// deduplicated, then scored with the hybrid formula and returned as top K.
//
// FTS returns up to 50 candidates by text relevance.
// Embedding scan returns up to 50 candidates by cosine similarity (positive only).
// Final scoring: relevance_score * 0.5 + quality_score * 0.3 + recency_score * 0.2
//
// relevance_score combines normalized FTS rank and embedding cosine:
//   - both sources: (normFTS + cosine) / 2, boosted 20% (capped at 1.0)
//   - FTS only:     normalized FTS rank
//   - cosine only:  cosine similarity
func (d *DB) hybridSearch(domain, query string, queryEmb []float32, limit int, focusAreas []string) ([]Learning, error) {
	type scored struct {
		learning Learning
		score    float64
	}

	// Step 1: FTS top 50 candidates — collect id → BM25 rank (negative float).
	ftsRanks := make(map[string]float64)
	if query != "" {
		ftsRows, err := d.db.Query(`
			SELECT l.id, fts.rank
			FROM learnings_fts fts
			JOIN learnings l ON fts.rowid = l.rowid
			WHERE learnings_fts MATCH ? AND l.domain = ? AND l.archived = 0
			ORDER BY rank
			LIMIT 50
		`, query, domain)
		if err == nil {
			for ftsRows.Next() {
				var id string
				var rank float64
				if ftsRows.Scan(&id, &rank) == nil {
					ftsRanks[id] = rank
				}
			}
			ftsRows.Close()
		}
	}

	// Normalize FTS ranks to [0, 1] where 1 = best match.
	normFTS := normalizeFTSRanks(ftsRanks)

	// Step 2: Load ALL domain rows, compute cosine similarity, take top 50.
	// Only positive cosines are retained; zero/negative cosines add no relevance signal.
	//
	// NOTE: This is O(n) in domain size. Acceptable at Nanika's scale (<5K learnings
	// per domain). Revisit this approach if any domain exceeds ~5K entries.
	embCosines := make(map[string]float64)
	if queryEmb != nil {
		type embResult struct {
			id  string
			sim float64
		}
		embResults := make([]embResult, 0, 512)
		embRows, err := d.db.Query(`
			SELECT id, embedding FROM learnings
			WHERE domain = ? AND archived = 0 AND embedding IS NOT NULL
				AND (injection_count < 20 OR compliance_rate >= 0.15)
				AND NOT (injection_count > 100 AND compliance_rate < 0.25)
		`, domain)
		if err == nil {
			for embRows.Next() {
				var id string
				var blob []byte
				if embRows.Scan(&id, &blob) == nil {
					emb := DecodeEmbedding(blob)
					if emb != nil {
						embResults = append(embResults, embResult{id, CosineSimilarity(queryEmb, emb)})
					}
				}
			}
			_ = embRows.Err() // best-effort; partial results still useful
			embRows.Close()
		}
		sort.Slice(embResults, func(i, j int) bool { return embResults[i].sim > embResults[j].sim })
		for _, r := range embResults[:min(len(embResults), 50)] {
			if r.sim > 0 {
				embCosines[r.id] = r.sim
			}
		}
	}

	// Step 3: Union — deduplicate by ID.
	allIDs := make([]string, 0, len(ftsRanks)+len(embCosines))
	for id := range ftsRanks {
		allIDs = append(allIDs, id)
	}
	for id := range embCosines {
		if _, ok := ftsRanks[id]; !ok {
			allIDs = append(allIDs, id)
		}
	}
	if len(allIDs) == 0 {
		return nil, nil
	}

	// Step 4: Fetch full rows for union candidates.
	// candidateCols must stay in sync with the Scan() call in scanLearning().
	const candidateCols = `id, type, content, context, domain, worker_name, workspace_id,
		tags, seen_count, used_count, quality_score, created_at, last_used_at, embedding,
		injection_count, compliance_rate`
	const candidateFilter = `archived = 0 AND (injection_count < 20 OR compliance_rate >= 0.15) AND NOT (injection_count > 100 AND compliance_rate < 0.25)`

	placeholders := strings.Repeat("?,", len(allIDs))
	placeholders = placeholders[:len(placeholders)-1]
	args := make([]any, 0, 1+len(allIDs))
	args = append(args, domain)
	for _, id := range allIDs {
		args = append(args, id)
	}
	rows, err := d.db.Query(`SELECT `+candidateCols+`
		FROM learnings
		WHERE domain = ? AND id IN (`+placeholders+`) AND `+candidateFilter, args...)
	if err != nil {
		return nil, fmt.Errorf("fetching union candidates: %w", err)
	}
	defer rows.Close()

	// Step 5: Score with hybrid formula:
	//   final = relevance * 0.5 + quality_score * 0.3 + recency_score * 0.2
	var candidates []scored
	for rows.Next() {
		l, err := scanLearning(rows)
		if err != nil {
			continue
		}
		relevance := computeRelevance(l.ID, normFTS, embCosines)
		score := relevance*0.5 + l.QualityScore*0.3 + recencyWeight(l.CreatedAt)*0.2
		if len(focusAreas) > 0 {
			boost := computeFocusBoost(l.Content, focusAreas)
			score *= (1.0 + 0.3*boost)
		}
		candidates = append(candidates, scored{l, score})
	}

	// Step 6: Sort and return top K above minimum relevance threshold.
	sort.Slice(candidates, func(i, j int) bool {
		return candidates[i].score > candidates[j].score
	})
	result := make([]Learning, 0, min(len(candidates), limit))
	for i := 0; i < len(candidates) && i < limit; i++ {
		if candidates[i].score < minRelevanceScore {
			break
		}
		result = append(result, candidates[i].learning)
	}
	return result, nil
}

// normalizeFTSRanks maps FTS5 BM25 ranks (negative floats, more negative = better)
// to [0, 1] scores where 1 = best match. Returns nil when ranks is empty.
func normalizeFTSRanks(ranks map[string]float64) map[string]float64 {
	if len(ranks) == 0 {
		return nil
	}
	var minRank, maxRank float64
	first := true
	for _, r := range ranks {
		if first {
			minRank, maxRank = r, r
			first = false
		} else {
			if r < minRank {
				minRank = r
			}
			if r > maxRank {
				maxRank = r
			}
		}
	}
	result := make(map[string]float64, len(ranks))
	span := maxRank - minRank
	for id, r := range ranks {
		if span == 0 {
			result[id] = 1.0
		} else {
			// minRank = most negative = best match → score 1.0
			// maxRank = least negative = worst match → score 0.0
			result[id] = (maxRank - r) / span
		}
	}
	return result
}

// computeRelevance returns a [0, 1] relevance score for candidate id by
// combining its normalized FTS rank and embedding cosine similarity.
// When both sources contribute, the average is boosted 20% (capped at 1.0).
func computeRelevance(id string, normFTS, cosines map[string]float64) float64 {
	fts, hasFTS := normFTS[id]
	cos, hasCos := cosines[id]
	switch {
	case hasFTS && hasCos:
		boosted := (fts + cos) / 2 * 1.2
		if boosted > 1 {
			return 1
		}
		return boosted
	case hasFTS:
		return fts
	case hasCos:
		return cos
	default:
		return 0
	}
}

// FindTopByQuality returns the top-K learnings for cold-start injection when
// no query context is available. The pipeline is:
//
//  1. SQL fetches 2×limit candidates ordered by quality_score × recency_tier.
//  2. A per-type cap of maxPerType prevents any LearningType from dominating.
//  3. MMR re-ranking (lambda = mmrLambda) balances relevance against topical
//     diversity using embedding cosine similarity — ties to duplicates get
//     penalized so distinct topics surface.
//  4. Final slice is truncated to limit.
//
// Applies the same compliance filter as FindRelevant
// (injection_count < 20 OR compliance_rate >= 0.15).
func (d *DB) FindTopByQuality(domain string, limit int) ([]Learning, error) {
	if limit <= 0 {
		limit = 20
	}

	// Fetch 2×limit candidates to give the per-type cap and MMR re-rank
	// headroom to drop near-duplicates and low-diversity picks without
	// starving the final slice.
	// Recency tiers mirror recencyWeight() — thresholds must be kept in sync.
	rows, err := d.db.Query(`
		SELECT id, type, content, context, domain, worker_name, workspace_id,
		       tags, seen_count, used_count, quality_score, created_at, last_used_at, embedding,
		       injection_count, compliance_rate
		FROM learnings
		WHERE domain = ? AND archived = 0
		  AND (injection_count < 20 OR compliance_rate >= 0.15)
		  AND NOT (injection_count > 100 AND compliance_rate < 0.25)
		ORDER BY quality_score * CASE
		    WHEN (julianday('now') - julianday(created_at)) < 30  THEN 1.0
		    WHEN (julianday('now') - julianday(created_at)) < 90  THEN 0.8
		    WHEN (julianday('now') - julianday(created_at)) < 180 THEN 0.6
		    ELSE 0.4
		END DESC
		LIMIT ?
	`, domain, 2*limit) // fetch 2 * limit to feed per-type cap + MMR
	if err != nil {
		return nil, fmt.Errorf("querying top by quality: %w", err)
	}
	defer rows.Close()

	var candidates []Learning
	for rows.Next() {
		l, err := scanLearning(rows)
		if err != nil {
			continue
		}
		candidates = append(candidates, l)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterating top by quality: %w", err)
	}

	capped := applyPerTypeCap(candidates, maxPerType)
	return mmrRerank(capped, limit), nil
}

// applyPerTypeCap returns a stable-ordered subset of candidates with at most
// perType entries per LearningType. Input order (quality-desc from SQL) is
// preserved so the per-type survivors are the highest-quality within each type.
func applyPerTypeCap(candidates []Learning, perType int) []Learning {
	if perType <= 0 || len(candidates) == 0 {
		return candidates
	}
	counts := make(map[LearningType]int, len(candidates))
	out := make([]Learning, 0, len(candidates))
	for _, l := range candidates {
		if counts[l.Type] >= perType {
			continue
		}
		counts[l.Type]++
		out = append(out, l)
	}
	return out
}

// mmrRerank applies Maximal Marginal Relevance re-ranking to pool, returning
// up to limit items. Relevance is quality_score × recency_weight; diversity is
// 1 - max(cosine similarity to any already-selected item). When a candidate
// has no embedding, its diversity cost falls back to 0 so legacy rows still
// participate in ranking.
//
// The algorithm is O(limit × len(pool)) which is fine for Nanika's scale
// (pool ≤ 2×limit, limit typically ≤ 30).
func mmrRerank(pool []Learning, limit int) []Learning {
	if limit <= 0 || len(pool) == 0 {
		return nil
	}
	if len(pool) <= limit {
		return pool
	}

	rel := make([]float64, len(pool))
	for i, l := range pool {
		rel[i] = l.QualityScore * recencyWeight(l.CreatedAt)
	}

	selected := make([]Learning, 0, limit)
	used := make([]bool, len(pool))
	for len(selected) < limit {
		bestIdx := -1
		var bestScore float64
		for i, cand := range pool {
			if used[i] {
				continue
			}
			var maxSim float64
			if len(cand.Embedding) > 0 {
				for _, s := range selected {
					if len(s.Embedding) == 0 {
						continue
					}
					sim := cosineSimilarity(cand.Embedding, s.Embedding)
					if sim > maxSim {
						maxSim = sim
					}
				}
			}
			score := mmrLambda*rel[i] - (1.0-mmrLambda)*maxSim
			if bestIdx == -1 || score > bestScore {
				bestIdx = i
				bestScore = score
			}
		}
		if bestIdx == -1 {
			break
		}
		used[bestIdx] = true
		selected = append(selected, pool[bestIdx])
	}
	return selected
}

// cosineSimilarity is a thin package-internal alias over CosineSimilarity,
// used by the MMR diversity re-ranker in FindTopByQuality.
func cosineSimilarity(a, b []float32) float64 {
	return CosineSimilarity(a, b)
}

// Stats returns database statistics.
func (d *DB) Stats() (total, withEmb int, err error) {
	d.db.QueryRow("SELECT COUNT(*) FROM learnings").Scan(&total)
	d.db.QueryRow("SELECT COUNT(*) FROM learnings WHERE embedding IS NOT NULL").Scan(&withEmb)
	return
}

func scanLearning(rows *sql.Rows) (Learning, error) {
	var l Learning
	var ltype, domain, workerName, workspaceID, tags string
	var createdAt string
	var lastUsedAt sql.NullString
	var embBlob []byte

	err := rows.Scan(
		&l.ID, &ltype, &l.Content, &l.Context, &domain, &workerName, &workspaceID,
		&tags, &l.SeenCount, &l.UsageCount, &l.QualityScore,
		&createdAt, &lastUsedAt, &embBlob,
		&l.InjectionCount, &l.ComplianceRate,
	)
	if err != nil {
		return l, err
	}

	l.Type = LearningType(ltype)
	l.Domain = domain
	l.WorkerName = workerName
	l.WorkspaceID = workspaceID
	if tags != "" {
		l.Tags = strings.Split(tags, ",")
	}
	if t, err := time.Parse(time.RFC3339, createdAt); err == nil {
		l.CreatedAt = t
	}
	if lastUsedAt.Valid && lastUsedAt.String != "" {
		if t, err := time.Parse(time.RFC3339, lastUsedAt.String); err == nil {
			l.LastUsedAt = &t
		}
	}
	l.Embedding = DecodeEmbedding(embBlob)
	return l, nil
}

// recencyWeight mirrors recencySQL above. Tier thresholds must be kept in sync.
func recencyWeight(createdAt time.Time) float64 {
	age := time.Since(createdAt)
	switch {
	case age < 30*24*time.Hour:
		return 1.0
	case age < 90*24*time.Hour:
		return 0.8
	case age < 180*24*time.Hour:
		return 0.6
	default:
		return 0.4
	}
}

func formatNullableTime(t *time.Time) interface{} {
	if t == nil {
		return nil
	}
	return t.UTC().Format(time.RFC3339)
}

// BackfillCandidate is one (id, content) pair selected for embedding backfill.
type BackfillCandidate struct {
	ID      string
	Content string
}

// BackfillStats summarizes the rows selected for embedding backfill.
type BackfillStats struct {
	Rows       int
	TotalChars int
}

// CountEmbeddingBackfill returns row count + total content bytes for rows
// where embedding IS NULL, optionally filtered by maxAge (only consider rows
// created within the window). When includeArchived is false, archived = 1
// rows are skipped. A zero maxAge means no age filter.
func (d *DB) CountEmbeddingBackfill(ctx context.Context, maxAge time.Duration, includeArchived bool) (BackfillStats, error) {
	q := `SELECT COUNT(*), COALESCE(SUM(LENGTH(content)), 0) FROM learnings WHERE embedding IS NULL`
	var args []any
	if !includeArchived {
		q += ` AND archived = 0`
	}
	if maxAge > 0 {
		q += ` AND created_at >= ?`
		args = append(args, time.Now().Add(-maxAge).UTC().Format(time.RFC3339))
	}
	var s BackfillStats
	if err := d.db.QueryRowContext(ctx, q, args...).Scan(&s.Rows, &s.TotalChars); err != nil {
		return BackfillStats{}, fmt.Errorf("count embedding backfill: %w", err)
	}
	return s, nil
}

// SelectEmbeddingBackfill returns up to limit rows that need an embedding,
// ordered by created_at ascending so resumed runs are deterministic.
// limit <= 0 returns every matching row.
func (d *DB) SelectEmbeddingBackfill(ctx context.Context, maxAge time.Duration, includeArchived bool, limit int) ([]BackfillCandidate, error) {
	q := `SELECT id, content FROM learnings WHERE embedding IS NULL`
	var args []any
	if !includeArchived {
		q += ` AND archived = 0`
	}
	if maxAge > 0 {
		q += ` AND created_at >= ?`
		args = append(args, time.Now().Add(-maxAge).UTC().Format(time.RFC3339))
	}
	q += ` ORDER BY created_at ASC`
	if limit > 0 {
		q += ` LIMIT ?`
		args = append(args, limit)
	}
	rows, err := d.db.QueryContext(ctx, q, args...)
	if err != nil {
		return nil, fmt.Errorf("select embedding backfill: %w", err)
	}
	defer rows.Close()

	var out []BackfillCandidate
	for rows.Next() {
		var c BackfillCandidate
		if err := rows.Scan(&c.ID, &c.Content); err != nil {
			return nil, fmt.Errorf("scan backfill candidate: %w", err)
		}
		out = append(out, c)
	}
	return out, rows.Err()
}

// SetEmbedding writes a vector to the learnings row identified by id, but
// only when the existing embedding column is still NULL. Returns true when
// a row was actually updated. The IS NULL guard makes the backfill safe to
// rerun and safe under concurrent embedding by another process.
func (d *DB) SetEmbedding(ctx context.Context, id string, emb []float32) (bool, error) {
	if len(emb) == 0 {
		return false, fmt.Errorf("set embedding for %s: refusing to write empty vector", id)
	}
	res, err := d.db.ExecContext(ctx,
		`UPDATE learnings SET embedding = ? WHERE id = ? AND embedding IS NULL`,
		EncodeEmbedding(emb), id)
	if err != nil {
		return false, fmt.Errorf("set embedding for %s: %w", id, err)
	}
	n, _ := res.RowsAffected()
	return n > 0, nil
}

// CleanupOptions controls learning database pruning behavior.
type CleanupOptions struct {
	MaxAgeDays   int     // delete unused learnings older than this (default: 180)
	MinScore     float64 // delete below this score regardless of age (default: 0.1)
	MaxPerDomain int     // cap per domain (default: 500)
	DryRun       bool    // print only, don't delete
}

// Cleanup prunes old, low-quality, or excess learnings.
// Returns the number of learnings removed (or that would be removed in dry-run).
func (d *DB) Cleanup(ctx context.Context, opts CleanupOptions) (int, error) {
	if opts.MaxAgeDays <= 0 {
		opts.MaxAgeDays = 180
	}
	if opts.MinScore <= 0 {
		opts.MinScore = 0.1
	}
	if opts.MaxPerDomain <= 0 {
		opts.MaxPerDomain = 500
	}

	// First, decay scores for unused learnings
	if !opts.DryRun {
		d.decayScores()
	}

	var totalRemoved int

	// 1. Age pruning: old + low quality + unused
	cutoff := time.Now().Add(-time.Duration(opts.MaxAgeDays) * 24 * time.Hour).UTC().Format(time.RFC3339)
	var ageCount int
	d.db.QueryRow(`SELECT COUNT(*) FROM learnings WHERE created_at < ? AND quality_score < 0.5 AND used_count = 0`, cutoff).Scan(&ageCount)

	if ageCount > 0 {
		if opts.DryRun {
			totalRemoved += ageCount
		} else {
			res, err := d.db.Exec(`DELETE FROM learnings WHERE created_at < ? AND quality_score < 0.5 AND used_count = 0`, cutoff)
			if err == nil {
				n, _ := res.RowsAffected()
				totalRemoved += int(n)
			}
		}
	}

	// 2. Score pruning: below absolute minimum regardless of age
	var scoreCount int
	d.db.QueryRow(`SELECT COUNT(*) FROM learnings WHERE quality_score < ?`, opts.MinScore).Scan(&scoreCount)

	if scoreCount > 0 {
		if opts.DryRun {
			totalRemoved += scoreCount
		} else {
			res, err := d.db.Exec(`DELETE FROM learnings WHERE quality_score < ?`, opts.MinScore)
			if err == nil {
				n, _ := res.RowsAffected()
				totalRemoved += int(n)
			}
		}
	}

	// 3. Count cap per domain
	rows, err := d.db.Query(`SELECT domain, COUNT(*) as cnt FROM learnings GROUP BY domain HAVING cnt > ?`, opts.MaxPerDomain)
	if err == nil {
		defer rows.Close()
		for rows.Next() {
			var dom string
			var cnt int
			if rows.Scan(&dom, &cnt) != nil {
				continue
			}
			excess := cnt - opts.MaxPerDomain
			if excess <= 0 {
				continue
			}
			if opts.DryRun {
				totalRemoved += excess
			} else {
				res, err := d.db.Exec(`DELETE FROM learnings WHERE id IN (
					SELECT id FROM learnings WHERE domain = ? ORDER BY quality_score ASC, created_at ASC LIMIT ?
				)`, dom, excess)
				if err == nil {
					n, _ := res.RowsAffected()
					totalRemoved += int(n)
				}
			}
		}
	}

	return totalRemoved, nil
}

// decayScores reduces quality_score over time using compliance-weighted rates:
//   - Never injected (injection_count = 0): 5% per 30d
//   - Low compliance (rate < 0.3):          15% per 30d
//   - Mid compliance (0.3 ≤ rate < 0.7):    5% per 30d
//   - High compliance (rate ≥ 0.7):         2% per 30d
func (d *DB) decayScores() {
	// Single pass: compliance-weighted decay applied in one table scan.
	// Rates: never-injected=5%, low-compliance(<0.3)=15%, mid=5%, high(≥0.7)=2%.
	d.db.Exec(`
		UPDATE learnings SET quality_score = quality_score * CASE
			WHEN injection_count = 0                                       THEN 0.95
			WHEN injection_count > 0 AND compliance_rate < 0.3             THEN 0.85
			WHEN injection_count > 0 AND compliance_rate >= 0.7            THEN 0.98
			ELSE                                                                0.95
		END
		WHERE created_at < datetime('now', '-30 days')
		AND quality_score > 0.05
	`)
}

// UpdateQualityScores recomputes quality_score for all non-archived learnings
// using the three-factor formula: base-tier × recency-decay × (1+injection boost) × compliance multiplier.
func (d *DB) UpdateQualityScores(ctx context.Context) (int, error) {
	res, err := d.db.ExecContext(ctx, `
		UPDATE learnings SET quality_score =
		  -- Base tier by type (from archived pre-v2 scoring formula)
		  (CASE type
		    WHEN 'insight'    THEN 1.0
		    WHEN 'decision'   THEN 0.8
		    WHEN 'pattern'    THEN 0.7
		    WHEN 'error'      THEN 0.6
		    WHEN 'source'     THEN 0.4
		    WHEN 'preference' THEN 0.3
		    WHEN 'behavior'   THEN 0.3
		    ELSE 0.5
		  END)
		  *
		  -- Recency decay (step-function; SQL-portable across sqlite versions)
		  (CASE
		    WHEN julianday('now') - julianday(created_at) <= 7   THEN 1.0
		    WHEN julianday('now') - julianday(created_at) <= 30  THEN 0.9
		    WHEN julianday('now') - julianday(created_at) <= 90  THEN 0.75
		    WHEN julianday('now') - julianday(created_at) <= 180 THEN 0.6
		    WHEN julianday('now') - julianday(created_at) <= 365 THEN 0.4
		    ELSE 0.25
		  END)
		  *
		  -- Injection boost (log-capped — prevents legacy cohort from reinforcing incumbency)
		  (1 + CASE
		    WHEN injection_count = 0  THEN 0
		    WHEN injection_count <= 2 THEN 0.2
		    WHEN injection_count <= 5 THEN 0.3
		    WHEN injection_count <= 10 THEN 0.4
		    ELSE 0.5
		  END)
		  *
		  -- Compliance multiplier (neutral 0.75 when no compliance data yet)
		  (CASE
		    WHEN compliance_count = 0 THEN 0.75
		    ELSE (0.5 + 0.5 * compliance_rate)
		  END)
		WHERE archived = 0
	`)
	if err != nil {
		return 0, fmt.Errorf("update quality scores: %w", err)
	}
	n, _ := res.RowsAffected()
	return int(n), nil
}

// computeFocusBoost returns 0.0-1.0 based on keyword overlap between
// learning content and persona focus areas.
func computeFocusBoost(content string, focusAreas []string) float64 {
	if len(focusAreas) == 0 {
		return 0
	}

	contentWords := tokenize(content)
	if len(contentWords) == 0 {
		return 0
	}

	var maxOverlap float64
	for _, area := range focusAreas {
		areaWords := tokenize(area)
		if len(areaWords) == 0 {
			continue
		}

		// Jaccard-like: count matching words / total unique words in focus area
		matches := 0
		for w := range areaWords {
			if contentWords[w] {
				matches++
			}
		}
		overlap := float64(matches) / float64(len(areaWords))
		if overlap > maxOverlap {
			maxOverlap = overlap
		}
	}
	return maxOverlap
}

// tokenize splits text into a lowercase word set, filtering short words.
func tokenize(text string) map[string]bool {
	words := make(map[string]bool)
	for _, w := range strings.Fields(strings.ToLower(text)) {
		w = strings.Trim(w, ".,;:!?\"'()-")
		if len(w) >= 4 {
			words[w] = true
		}
	}
	return words
}

func DefaultDBPath() string {
	base, err := config.Dir()
	if err != nil {
		home, _ := os.UserHomeDir()
		// Check .alluka first, fall back to .via
		alluka := filepath.Join(home, ".alluka", "learnings.db")
		if _, statErr := os.Stat(filepath.Join(home, ".alluka")); statErr == nil {
			return alluka
		}
		return filepath.Join(home, ".via", "learnings.db")
	}
	return filepath.Join(base, "learnings.db")
}

// ArchiveOptions controls ArchiveDeadWeight behavior.
type ArchiveOptions struct {
	DryRun bool   // when true, report candidates without writing to DB
	Domain string // empty = all domains
}

// ArchiveCandidate describes a single learning flagged for archival.
type ArchiveCandidate struct {
	ID     string
	Reason string
}

// ArchiveDeadWeight identifies (and optionally archives) learnings that are no
// longer useful. Four criteria are applied:
//
//  1. Never injected + old (> 90 days) — relevant content would have been
//     picked up by hybridSearch; this one never was.
//  2. Chronic non-compliance: injection_count >= 5 and compliance_rate < 0.10
//     — workers have repeatedly received this learning and consistently ignored it.
//  3. Low quality + never used + old (> 60 days) — quality_score < 0.2 and
//     used_count = 0; scored poorly at capture and never applied.
//  4. Single observation, no embedding, old (> 30 days) — seen_count = 1 and
//     embedding IS NULL; likely a noisy one-off that was never reinforced.
//
// Returns the list of candidates. When opts.DryRun is false the matching rows
// are updated to set archived = 1.
func (d *DB) ArchiveDeadWeight(ctx context.Context, opts ArchiveOptions) ([]ArchiveCandidate, error) {
	type criterion struct {
		reason string
		query  string
		args   []any
	}

	domainFilter := ""
	var domainArg []any
	if opts.Domain != "" {
		domainFilter = " AND domain = ?"
		domainArg = []any{opts.Domain}
	}

	criteria := []criterion{
		{
			reason: "never injected, older than 90 days",
			query: `SELECT id FROM learnings WHERE archived = 0
				AND injection_count = 0 AND used_count = 0
				AND created_at < datetime('now', '-90 days')` + domainFilter,
			args: domainArg,
		},
		{
			reason: "chronic non-compliance (injection_count >= 5, compliance_rate < 0.10)",
			query: `SELECT id FROM learnings WHERE archived = 0
				AND injection_count >= 5 AND compliance_rate < 0.10` + domainFilter,
			args: domainArg,
		},
		{
			reason: "low quality, never used, older than 60 days",
			query: `SELECT id FROM learnings WHERE archived = 0
				AND quality_score < 0.2 AND used_count = 0
				AND created_at < datetime('now', '-60 days')` + domainFilter,
			args: domainArg,
		},
		{
			reason: "single observation, no embedding, older than 30 days",
			query: `SELECT id FROM learnings WHERE archived = 0
				AND seen_count = 1 AND embedding IS NULL
				AND created_at < datetime('now', '-30 days')` + domainFilter,
			args: domainArg,
		},
		{
			reason: "stale_injected",
			query: `SELECT id FROM learnings WHERE archived = 0
				AND injection_count > 100 AND compliance_rate < 0.25` + domainFilter,
			args: domainArg,
		},
	}

	seen := make(map[string]bool)
	var candidates []ArchiveCandidate

	for _, c := range criteria {
		rows, err := d.db.QueryContext(ctx, c.query, c.args...)
		if err != nil {
			return nil, fmt.Errorf("archive query (%s): %w", c.reason, err)
		}
		for rows.Next() {
			var id string
			if err := rows.Scan(&id); err != nil {
				rows.Close()
				return nil, fmt.Errorf("scanning archive candidate: %w", err)
			}
			if !seen[id] {
				seen[id] = true
				candidates = append(candidates, ArchiveCandidate{ID: id, Reason: c.reason})
			}
		}
		rows.Close()
	}

	if opts.DryRun || len(candidates) == 0 {
		return candidates, nil
	}

	tx, err := d.db.BeginTx(ctx, nil)
	if err != nil {
		return nil, fmt.Errorf("begin archive transaction: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck

	for _, c := range candidates {
		if _, err := tx.ExecContext(ctx,
			"UPDATE learnings SET archived = 1 WHERE id = ?", c.ID); err != nil {
			return nil, fmt.Errorf("archiving learning %s: %w", c.ID, err)
		}
	}
	if err := tx.Commit(); err != nil {
		return nil, fmt.Errorf("commit archive: %w", err)
	}
	return candidates, nil
}

// RecordInjections increments injection_count for each learning ID in the list.
// Called when learnings are injected into a worker's context bundle.
func (d *DB) RecordInjections(ctx context.Context, ids []string) error {
	for _, id := range ids {
		if _, err := d.db.ExecContext(ctx,
			"UPDATE learnings SET injection_count = injection_count + 1, last_used_at = datetime('now') WHERE id = ?", id); err != nil {
			return fmt.Errorf("recording injection for %s: %w", id, err)
		}
	}
	return nil
}

// RecordCompliance updates compliance tracking for a single learning after a mission.
// followed=true means the heuristic scan detected the learning was applied.
// It increments compliance_count if followed, then recomputes compliance_rate.
func (d *DB) RecordCompliance(ctx context.Context, id string, followed bool) error {
	if followed {
		if _, err := d.db.ExecContext(ctx,
			"UPDATE learnings SET compliance_count = compliance_count + 1 WHERE id = ?", id); err != nil {
			return fmt.Errorf("incrementing compliance_count for %s: %w", id, err)
		}
	}
	// Recompute compliance_rate = compliance_count / injection_count (only when injection_count > 0)
	if _, err := d.db.ExecContext(ctx, `
		UPDATE learnings
		SET compliance_rate = CAST(compliance_count AS REAL) / injection_count
		WHERE id = ? AND injection_count > 0
	`, id); err != nil {
		return fmt.Errorf("recomputing compliance_rate for %s: %w", id, err)
	}
	return nil
}

// ComplianceStats returns aggregate compliance metrics across all learnings.
// injected = number of learnings that have been injected at least once.
// avgRate = average compliance_rate across injected learnings (0.0 if none).
func (d *DB) ComplianceStats() (total, injected int, avgRate float64, err error) {
	d.db.QueryRow("SELECT COUNT(*) FROM learnings").Scan(&total)
	d.db.QueryRow("SELECT COUNT(*) FROM learnings WHERE injection_count > 0").Scan(&injected)
	if injected > 0 {
		d.db.QueryRow("SELECT AVG(compliance_rate) FROM learnings WHERE injection_count > 0").Scan(&avgRate)
	}
	return
}

// FindPromotable returns learnings with quality_score > 0.7 that have not yet been promoted.
// Sorted by quality_score descending for prioritization.
func (d *DB) FindPromotable(ctx context.Context) ([]Learning, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT id, type, content, context, domain, worker_name, workspace_id,
		       tags, seen_count, used_count, quality_score, created_at, last_used_at, embedding,
		       injection_count, compliance_rate
		FROM learnings
		WHERE quality_score > 0.7 AND promoted_at IS NULL AND archived = 0
		ORDER BY quality_score DESC
	`)
	if err != nil {
		return nil, fmt.Errorf("querying promotable learnings: %w", err)
	}
	defer rows.Close()

	var result []Learning
	for rows.Next() {
		l, err := scanLearning(rows)
		if err != nil {
			continue
		}
		result = append(result, l)
	}
	return result, rows.Err()
}

// MarkPromoted marks a learning as promoted by setting promoted_at to now.
func (d *DB) MarkPromoted(ctx context.Context, id string) error {
	_, err := d.db.ExecContext(ctx,
		"UPDATE learnings SET promoted_at = datetime('now') WHERE id = ?", id)
	if err != nil {
		return fmt.Errorf("marking learning %s as promoted: %w", id, err)
	}
	return nil
}
