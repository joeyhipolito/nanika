package zettel

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// DailyNote represents the daily summary note that records completed missions
// in order. Implementation pending T1.4 (TRK-525 Phase 1).
type DailyNote struct {
	Date     string   // ISO date, e.g. "2026-04-20"
	Missions []string // mission IDs in completion order
}

var dailyLocks sync.Map // per-daily-file mutex for concurrent append safety

// RenderDaily renders a daily note with frontmatter and sections.
func RenderDaily(date time.Time) string {
	dateStr := date.UTC().Format("2006-01-02")
	return fmt.Sprintf("---\ntype: daily\ndate: %s\n---\n\n# %s\n\n## Missions\n\n", dateStr, dateStr)
}

// AppendMissionToDaily appends a mission wikilink to a daily note's "## Missions" section.
// Creates the daily note if it doesn't exist.
// Idempotent: no-op if the wikilink already exists.
func AppendMissionToDaily(vaultPath string, date time.Time, missionWikilink string, kinds ...vault.VaultKind) error {
	kind := vault.KindNanika
	if len(kinds) > 0 {
		kind = kinds[0]
	}
	schema := vault.SchemaFor(kind)
	dateStr := date.UTC().Format("2006-01-02")
	dailyPath := filepath.Join(vaultPath, schema.Daily, dateStr+".md")

	// Get per-file lock
	mu, _ := dailyLocks.LoadOrStore(dailyPath, &sync.Mutex{})
	mu.(*sync.Mutex).Lock()
	defer mu.(*sync.Mutex).Unlock()

	// Check if file exists
	data, err := os.ReadFile(dailyPath)
	var content string
	if err != nil {
		// File doesn't exist, create it
		content = RenderDaily(date)
		// Create parent directories
		dir := filepath.Dir(dailyPath)
		if err := os.MkdirAll(dir, 0755); err != nil {
			return fmt.Errorf("cannot create daily directory: %w", err)
		}
	} else {
		content = string(data)
	}

	// Find "## Missions" section
	sectionMarker := "## Missions"
	idx := strings.Index(content, sectionMarker)
	if idx == -1 {
		return fmt.Errorf("cannot find '%s' section", sectionMarker)
	}

	// Find the end of the line containing the marker
	lineEnd := strings.Index(content[idx:], "\n")
	if lineEnd == -1 {
		lineEnd = len(content)
	} else {
		lineEnd = idx + lineEnd + 1
	}

	// Find the next heading at same or shallower depth
	restContent := content[lineEnd:]
	nextHeading := -1
	lines := strings.Split(restContent, "\n")
	for i, line := range lines {
		trimmed := strings.TrimRight(line, " \t")
		if strings.HasPrefix(trimmed, "## ") || strings.HasPrefix(trimmed, "# ") {
			nextHeading = i
			break
		}
	}

	// Calculate insert offset
	insertOffset := len(content)
	if nextHeading >= 0 {
		// Find byte offset of the next heading
		offset := lineEnd
		for i := 0; i < nextHeading; i++ {
			offset += len(lines[i]) + 1 // +1 for newline
		}
		insertOffset = offset
	}

	// Check if wikilink already present in this section
	section := content[lineEnd:insertOffset]
	wikilinksToCheck := []string{
		fmt.Sprintf("[[%s]]", missionWikilink),
		fmt.Sprintf("[[%s|", missionWikilink),
	}
	for _, linkFmt := range wikilinksToCheck {
		if strings.Contains(section, linkFmt) {
			return nil // already present, idempotent no-op
		}
	}

	// Append the wikilink
	newLine := fmt.Sprintf("- [[%s]]\n", missionWikilink)
	newContent := content[:insertOffset] + newLine + content[insertOffset:]

	// Write back atomically
	tmpPath := filepath.Join(filepath.Dir(dailyPath), "."+filepath.Base(dailyPath)+".tmp")
	if err := os.WriteFile(tmpPath, []byte(newContent), 0644); err != nil {
		return fmt.Errorf("cannot write temp file: %w", err)
	}
	if err := os.Rename(tmpPath, dailyPath); err != nil {
		os.Remove(tmpPath)
		return fmt.Errorf("cannot rename: %w", err)
	}

	return nil
}
