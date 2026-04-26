package learning

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"time"
)

// IngestStats reports the outcome of an IngestDocs run.
type IngestStats struct {
	FilesScanned       int
	ChunksCreated      int
	ChunksSkippedDedup int
	Errors             []string
}

const (
	ingestDomain     = "dev"
	ingestWorkerName = "docs-ingest"
	ingestMaxBody    = 800
)

var slugNonAlphanumHyphen = regexp.MustCompile(`[^a-z0-9-]+`)

// IngestDocs walks root, splits each .md file by ## headings into chunks,
// and inserts them as TypeSource learnings into db. embedder is forwarded to
// DB.Insert; pass nil when no API key is configured to use the legacy
// null-embedding path.
// Per-file errors are accumulated in IngestStats.Errors; only an unreadable
// root returns a non-nil error.
func IngestDocs(root string, db *DB, embedder *Embedder) (IngestStats, error) {
	if _, err := os.Stat(root); err != nil {
		return IngestStats{}, fmt.Errorf("ingest root: %w", err)
	}

	var stats IngestStats
	ctx := context.Background()

	err := filepath.WalkDir(root, func(path string, d fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			stats.Errors = append(stats.Errors, fmt.Sprintf("%s: %v", path, walkErr))
			return nil
		}
		if d.IsDir() || !strings.HasSuffix(strings.ToLower(d.Name()), ".md") {
			return nil
		}

		rel, _ := filepath.Rel(root, path)
		stats.FilesScanned++

		data, err := os.ReadFile(path)
		if err != nil {
			stats.Errors = append(stats.Errors, fmt.Sprintf("%s: %v", rel, err))
			return nil
		}

		stem := strings.ToLower(strings.TrimSuffix(filepath.Base(path), ".md"))

		for _, chunk := range splitByHeading(string(data), rel) {
			body := strings.TrimSpace(chunk.body)
			if len(body) > ingestMaxBody {
				body = body[:ingestMaxBody]
			}
			if body == "" {
				continue
			}

			id := docChunkID(chunk.ctx, ingestDomain)

			// Pre-check existence so we can count deduped chunks accurately.
			var existingID string
			lookupErr := db.db.QueryRowContext(ctx,
				"SELECT id FROM learnings WHERE id = ?", id).Scan(&existingID)
			if lookupErr == nil {
				stats.ChunksSkippedDedup++
				continue
			}

			l := Learning{
				ID:           id,
				Type:         TypeSource,
				Content:      body,
				Context:      chunk.ctx,
				Domain:       ingestDomain,
				WorkerName:   ingestWorkerName,
				WorkspaceID:  "",
				Marker:       "SOURCE:",
				Tags:         []string{"docs", stem},
				CreatedAt:    time.Now(),
				QualityScore: 0.5,
			}
			if err := db.Insert(ctx, l, embedder); err != nil {
				stats.Errors = append(stats.Errors, fmt.Sprintf("%s: insert: %v", rel, err))
				continue
			}
			stats.ChunksCreated++
		}
		return nil
	})
	if err != nil {
		return stats, fmt.Errorf("walking %s: %w", root, err)
	}
	return stats, nil
}

type mdChunk struct {
	ctx  string // context field: "rel-path#slug"
	body string
}

// splitByHeading splits a markdown document by ## headings.
// Text before the first heading becomes a preamble chunk.
// Files with no ## headings produce a single preamble chunk.
func splitByHeading(content, relPath string) []mdChunk {
	lines := strings.Split(content, "\n")

	var chunks []mdChunk
	var currentHeading string
	var buf strings.Builder
	hasHeading := false

	flush := func() {
		text := buf.String()
		buf.Reset()
		var slug string
		if !hasHeading && currentHeading == "" {
			slug = "_preamble"
		} else {
			slug = headingToSlug(currentHeading)
		}
		chunks = append(chunks, mdChunk{
			ctx:  relPath + "#" + slug,
			body: text,
		})
	}

	for _, line := range lines {
		if strings.HasPrefix(line, "## ") {
			if hasHeading || buf.Len() > 0 {
				flush()
			}
			currentHeading = strings.TrimPrefix(line, "## ")
			hasHeading = true
			continue
		}
		buf.WriteString(line)
		buf.WriteByte('\n')
	}

	// Flush the last (or only) section.
	if hasHeading || buf.Len() > 0 {
		flush()
	}

	// Empty file — produce one preamble chunk so the caller can decide to skip.
	if len(chunks) == 0 {
		chunks = []mdChunk{{ctx: relPath + "#_preamble", body: content}}
	}

	return chunks
}

// headingToSlug converts a ## heading text to a URL-safe slug:
// lowercase, spaces → hyphens, strip non-alphanumeric/hyphen chars.
func headingToSlug(heading string) string {
	s := strings.ToLower(strings.TrimSpace(heading))
	s = strings.ReplaceAll(s, " ", "-")
	s = slugNonAlphanumHyphen.ReplaceAllString(s, "")
	return strings.Trim(s, "-")
}

// docChunkID produces a deterministic ID from the chunk context and domain.
// Determinism enables INSERT OR IGNORE dedup on re-ingest.
func docChunkID(chunkCtx, domain string) string {
	h := sha256.Sum256([]byte(chunkCtx + "|" + domain))
	return "docs_" + hex.EncodeToString(h[:16])
}
