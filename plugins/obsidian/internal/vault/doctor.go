// Package vault implements RFC §7 Phase 0 (TRK-524): vault health diagnostics.
package vault

import (
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
)

// Report holds the results of a vault health check.
type Report struct {
	Path                string   `json:"path"`
	OrphanCount         int      `json:"orphan_count"`
	DanglingCount       int      `json:"dangling_count"`
	InvariantViolations []string `json:"invariant_violations,omitempty"`
	Issues              []string `json:"issues,omitempty"`
}

// Doctor walks the vault rooted at path and returns a diagnostic report.
// It detects dangling wikilinks (links to notes that do not exist in the vault).
// Orphan detection is Phase 4 (T4.3) and is not performed here.
func Doctor(path string) (Report, error) {
	report := Report{Path: path}

	info, err := os.Stat(path)
	if err != nil {
		return report, fmt.Errorf("accessing vault: %w", err)
	}
	if !info.IsDir() {
		return report, fmt.Errorf("not a directory: %s", path)
	}

	// Collect all note base names (filename without .md extension).
	noteNames := make(map[string]bool)
	var notePaths []string
	err = filepath.WalkDir(path, func(p string, d fs.DirEntry, err error) error {
		if err != nil {
			return nil // skip inaccessible entries
		}
		if d.IsDir() && strings.HasPrefix(d.Name(), ".") {
			return filepath.SkipDir
		}
		if !d.IsDir() && strings.HasSuffix(d.Name(), ".md") {
			base := strings.TrimSuffix(d.Name(), ".md")
			noteNames[base] = true
			notePaths = append(notePaths, p)
		}
		return nil
	})
	if err != nil {
		return report, fmt.Errorf("walking vault: %w", err)
	}

	// Check each note for dangling wikilinks.
	for _, notePath := range notePaths {
		data, err := os.ReadFile(notePath)
		if err != nil {
			continue
		}
		note := ParseNote(string(data))
		for _, link := range note.Wikilinks {
			if !noteNames[link] {
				report.DanglingCount++
			}
		}
	}

	return report, nil
}
