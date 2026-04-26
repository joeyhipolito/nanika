package zettel

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// IdeaZettel represents an automatically created idea note that is seeded
// from a mission's idea field and updated by subsequent missions that reference
// the same idea. Implementation pending T1.3 (TRK-525 Phase 1).
type IdeaZettel struct {
	ID      string
	Content string
}

var ideaLocks sync.Map // per-idea-file mutex for concurrent append safety

// EnsureIdeaExists creates an idea zettel if it doesn't exist.
// Returns (created, error).
func EnsureIdeaExists(vaultPath, slug string, kinds ...vault.VaultKind) (bool, error) {
	if slug == "" {
		return false, nil
	}
	kind := vault.KindNanika
	if len(kinds) > 0 {
		kind = kinds[0]
	}
	schema := vault.SchemaFor(kind)

	ideaPath := filepath.Join(vaultPath, schema.Ideas, slug+".md")

	// Check if already exists
	if _, err := os.Stat(ideaPath); err == nil {
		return false, nil // already exists
	}

	// Create parent directories
	dir := filepath.Dir(ideaPath)
	if err := os.MkdirAll(dir, 0755); err != nil {
		return false, fmt.Errorf("cannot create ideas directory: %w", err)
	}

	// Use O_CREATE|O_EXCL to ensure atomic creation (fail if exists)
	// We create the file and write the placeholder content
	content := fmt.Sprintf("---\ntype: idea\nslug: %s\n---\n\n# %s\n\n## Active missions\n\n", slug, slug)
	f, err := os.OpenFile(ideaPath, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0644)
	if err != nil {
		if os.IsExist(err) {
			return false, nil // race: another writer created it
		}
		return false, fmt.Errorf("cannot create idea file: %w", err)
	}
	defer f.Close()

	if _, err := f.WriteString(content); err != nil {
		return false, fmt.Errorf("cannot write idea file: %w", err)
	}

	return true, nil
}

// AppendMissionToIdea appends a mission wikilink to an idea's "## Active missions" section.
// Idempotent: no-op if the wikilink already exists.
func AppendMissionToIdea(vaultPath, ideaSlug, missionWikilink string, kinds ...vault.VaultKind) error {
	if ideaSlug == "" {
		return nil
	}
	kind := vault.KindNanika
	if len(kinds) > 0 {
		kind = kinds[0]
	}
	schema := vault.SchemaFor(kind)

	ideaPath := filepath.Join(vaultPath, schema.Ideas, ideaSlug+".md")

	// Get per-file lock
	mu, _ := ideaLocks.LoadOrStore(ideaPath, &sync.Mutex{})
	mu.(*sync.Mutex).Lock()
	defer mu.(*sync.Mutex).Unlock()

	// Read the file
	data, err := os.ReadFile(ideaPath)
	if err != nil {
		return fmt.Errorf("cannot read idea file: %w", err)
	}
	content := string(data)

	// Find "## Active missions" section
	sectionMarker := "## Active missions"
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
	tmpPath := filepath.Join(filepath.Dir(ideaPath), "."+filepath.Base(ideaPath)+".tmp")
	if err := os.WriteFile(tmpPath, []byte(newContent), 0644); err != nil {
		return fmt.Errorf("cannot write temp file: %w", err)
	}
	if err := os.Rename(tmpPath, ideaPath); err != nil {
		os.Remove(tmpPath)
		return fmt.Errorf("cannot rename: %w", err)
	}

	return nil
}
