package worker

import (
	"context"
	"fmt"

	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
)

// SearchLearnings searches the learning database using FindRelevant with FTS5.
// Returns formatted results as [quality] date persona: content, up to limit entries.
func SearchLearnings(ctx context.Context, query string, limit int) ([]string, error) {
	if limit <= 0 {
		limit = 10
	}

	// Open learning database
	db, err := learning.OpenDB("")
	if err != nil {
		return nil, fmt.Errorf("open learning DB: %w", err)
	}
	defer db.Close()

	// Search using FindRelevant (uses hybrid FTS5 + semantic search)
	// Empty domain means search across all domains
	learnings, err := db.FindRelevant(ctx, query, "", limit, nil)
	if err != nil {
		return nil, fmt.Errorf("find relevant learnings: %w", err)
	}

	if len(learnings) == 0 {
		return nil, nil
	}

	// Format results as [quality] date persona: content
	var results []string
	for _, l := range learnings {
		formatted := formatSearchResult(l)
		results = append(results, formatted)
	}

	return results, nil
}

// formatSearchResult formats a learning as [quality] date persona: content
func formatSearchResult(l learning.Learning) string {
	quality := fmt.Sprintf("%.2f", l.QualityScore)
	date := l.CreatedAt.Format("2006-01-02")
	persona := l.WorkerName
	if persona == "" {
		persona = "unknown"
	}

	// Truncate content to first line if multiline
	content := l.Content
	if idx := indexFirstNewline(content); idx >= 0 {
		content = content[:idx]
	}

	return fmt.Sprintf("[%s] %s %s: %s", quality, date, persona, content)
}

// indexFirstNewline returns the index of the first newline, or -1 if not found
func indexFirstNewline(s string) int {
	for i := 0; i < len(s); i++ {
		if s[i] == '\n' {
			return i
		}
	}
	return -1
}
