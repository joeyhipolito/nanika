package main

import (
	"encoding/json"
	"fmt"
	"strings"

	"github.com/joeyhipolito/nanika-obsidian/internal/recall"
)

// formatRecall formats recall results according to the specified format.
func formatRecall(results []recall.WalkResult, format string) (string, error) {
	switch format {
	case "json":
		return formatJSON(results)
	case "markdown":
		return formatMarkdown(results)
	case "paths":
		return formatPaths(results)
	case "brief":
		return formatBrief(results)
	default:
		return "", fmt.Errorf("unknown format: %q", format)
	}
}

// formatJSON returns results as JSON.
func formatJSON(results []recall.WalkResult) (string, error) {
	data, err := json.MarshalIndent(results, "", "  ")
	if err != nil {
		return "", err
	}
	return string(data) + "\n", nil
}

// formatMarkdown returns results as a Markdown list with paths and scores.
func formatMarkdown(results []recall.WalkResult) (string, error) {
	if len(results) == 0 {
		return "", nil
	}

	var buf strings.Builder
	for _, r := range results {
		// Format: - path (score: 0.95)
		fmt.Fprintf(&buf, "- %s (score: %.2f)\n", r.Path, r.Score)
	}
	return buf.String(), nil
}

// formatPaths returns results as one path per line with no scores.
func formatPaths(results []recall.WalkResult) (string, error) {
	if len(results) == 0 {
		return "", nil
	}

	var buf strings.Builder
	for _, r := range results {
		fmt.Fprintf(&buf, "%s\n", r.Path)
	}
	return buf.String(), nil
}

// formatBrief returns a compact single-line summary of results.
func formatBrief(results []recall.WalkResult) (string, error) {
	if len(results) == 0 {
		return "", nil
	}

	paths := make([]string, len(results))
	for i, r := range results {
		paths[i] = r.Path
	}
	return strings.Join(paths, ", "), nil
}
