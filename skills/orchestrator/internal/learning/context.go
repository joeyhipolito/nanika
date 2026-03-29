package learning

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// FlushContext queries relevant learnings and writes them as a context bundle to outputPath.
// The bundle is a markdown file suitable for injection into a worker's system prompt.
// Returns an error if the query fails or the file cannot be written.
func FlushContext(ctx context.Context, db *DB, embedder *Embedder, query, domain string, limit int, outputPath string) error {
	content, err := InjectContext(ctx, db, embedder, query, domain, limit)
	if err != nil {
		return err
	}

	if err := os.MkdirAll(filepath.Dir(outputPath), 0700); err != nil {
		return fmt.Errorf("creating output directory: %w", err)
	}

	// Atomic write: temp file then rename to avoid partial writes on crash
	tmpPath := outputPath + ".tmp"
	if err := os.WriteFile(tmpPath, []byte(content), 0600); err != nil {
		return fmt.Errorf("writing temp file: %w", err)
	}
	return os.Rename(tmpPath, outputPath)
}

// InjectContext retrieves relevant learnings and formats them as a markdown context block
// ready for prepending to a worker prompt. Records injection counts for compliance tracking.
// Returns an empty string when no relevant learnings are found.
func InjectContext(ctx context.Context, db *DB, embedder *Embedder, query, domain string, limit int) (string, error) {
	if limit <= 0 {
		limit = 10
	}

	learnings, err := db.FindRelevant(ctx, query, domain, limit, embedder)
	if err != nil {
		return "", fmt.Errorf("finding relevant learnings: %w", err)
	}
	if len(learnings) == 0 {
		return "", nil
	}

	var sb strings.Builder
	sb.WriteString("## Relevant Learnings\n\n")
	for _, l := range learnings {
		sb.WriteString(fmt.Sprintf("- **[%s]** %s\n", l.Type, l.Content))
	}
	sb.WriteString("\n")

	ids := make([]string, len(learnings))
	for i, l := range learnings {
		ids[i] = l.ID
	}
	_ = db.RecordInjections(ctx, ids)

	return sb.String(), nil
}

// SnapshotSession captures learnings from all worker output.md files in a workspace
// and stores them in the database. Returns the number of learnings captured.
func SnapshotSession(ctx context.Context, db *DB, embedder *Embedder, wsPath, domain string) (int, error) {
	workersDir := filepath.Join(wsPath, "workers")
	workers, err := os.ReadDir(workersDir)
	if err != nil {
		if os.IsNotExist(err) {
			return 0, nil
		}
		return 0, fmt.Errorf("reading workers dir: %w", err)
	}

	wsID := filepath.Base(wsPath)
	var total int

	for _, w := range workers {
		if !w.IsDir() {
			continue
		}

		data, err := os.ReadFile(filepath.Join(workersDir, w.Name(), "output.md"))
		if err != nil {
			continue
		}

		for _, l := range CaptureFromText(string(data), w.Name(), domain, wsID) {
			if err := db.Insert(ctx, l, embedder); err == nil {
				total++
			}
		}
	}

	return total, nil
}
