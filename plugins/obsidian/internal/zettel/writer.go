// writer.go — RFC §7 (TRK-525 Phase 1B): Atomic file writes with deduplication and idea/daily append.
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

// WriteResult reports the outcome of a write operation.
type WriteResult struct {
	Path       string
	Skipped    bool
	SkipReason string
	// Warnings captures non-fatal errors from idea/daily append steps.
	Warnings []string
}

// Writer handles mission zettel writes with atomic operations and deduplication.
type Writer struct {
	VaultPath    string
	Schema       vault.Schema
	kind         vault.VaultKind
	Dedup        *DedupDB
	missionLocks sync.Map // per-mission-ID mutex to serialize check→write→record
}

// NewWriter creates a writer for a vault. Pass a vault.VaultKind to select a non-default schema.
func NewWriter(vaultPath string, kinds ...vault.VaultKind) (*Writer, error) {
	kind := vault.KindNanika
	if len(kinds) > 0 {
		kind = kinds[0]
	}
	cacheDir := filepath.Join(vaultPath, ".cache")
	if err := os.MkdirAll(cacheDir, 0755); err != nil {
		return nil, fmt.Errorf("cannot create cache dir: %w", err)
	}

	dedupDB, err := OpenDedupDB(filepath.Join(cacheDir, "write-dedup.db"))
	if err != nil {
		return nil, fmt.Errorf("cannot open dedup db: %w", err)
	}

	return &Writer{
		VaultPath: vaultPath,
		Schema:    vault.SchemaFor(kind),
		kind:      kind,
		Dedup:     dedupDB,
	}, nil
}

// WriteMission writes a mission zettel atomically, then appends to the daily note
// and (when ideaSlug is non-empty) ensures the idea zettel exists and backlinks it.
// Idea/daily errors are non-fatal: they accumulate in WriteResult.Warnings.
// A per-ID mutex serializes the check → write → record sequence to prevent
// a TOCTOU race where two concurrent callers both observe exists=false, both
// write the file, and the second RecordWrite hits a UNIQUE constraint error.
func (w *Writer) WriteMission(id, slug, ideaSlug string, m Mission) (WriteResult, error) {
	if id == "" {
		return WriteResult{}, fmt.Errorf("mission id must not be empty")
	}

	// Serialize per mission ID to prevent TOCTOU race.
	mu := w.getMissionLock(id)
	mu.Lock()
	defer mu.Unlock()

	// Check dedup
	exists, existingPath, err := w.Dedup.HasMission(id)
	if err != nil {
		return WriteResult{}, fmt.Errorf("dedup check failed: %w", err)
	}
	if exists {
		return WriteResult{
			Path:       existingPath,
			Skipped:    true,
			SkipReason: "duplicate mission_id",
		}, nil
	}

	// Render the mission
	content := RenderMission(m)

	// Determine output path
	date := ""
	if !m.Completed.IsZero() {
		date = m.Completed.UTC().Format("2006-01-02")
	}
	if date == "" {
		date = time.Now().UTC().Format("2006-01-02")
	}
	var filename string
	if slug == "" {
		filename = fmt.Sprintf("%s-%s.md", date, id)
	} else {
		filename = fmt.Sprintf("%s-%s.md", date, slug)
	}
	targetPath := filepath.Join(w.VaultPath, w.Schema.Missions, filename)

	// Write atomically (write-to-tmp + rename)
	if err := w.atomicWrite(targetPath, content); err != nil {
		return WriteResult{}, fmt.Errorf("atomic write failed: %w", err)
	}

	// Record in dedup
	if err := w.Dedup.RecordWrite(id, targetPath); err != nil {
		return WriteResult{}, fmt.Errorf("cannot record write: %w", err)
	}

	result := WriteResult{Path: targetPath}

	// Determine effective date for daily append
	effectiveDate := m.Completed
	if effectiveDate.IsZero() {
		effectiveDate = time.Now().UTC()
	}

	// Wikilink stem = filename without .md extension
	wikilink := strings.TrimSuffix(filepath.Base(targetPath), ".md")

	// Non-fatal: append to daily note
	if err := AppendMissionToDaily(w.VaultPath, effectiveDate, wikilink, w.kind); err != nil {
		result.Warnings = append(result.Warnings, "AppendMissionToDaily: "+err.Error())
	}

	// Non-fatal: idea orchestration — skip cleanly when ideaSlug is empty
	if ideaSlug != "" {
		if _, err := EnsureIdeaExists(w.VaultPath, ideaSlug, w.kind); err != nil {
			result.Warnings = append(result.Warnings, "EnsureIdeaExists: "+err.Error())
		} else if err := AppendMissionToIdea(w.VaultPath, ideaSlug, wikilink, w.kind); err != nil {
			result.Warnings = append(result.Warnings, "AppendMissionToIdea: "+err.Error())
		}
	}

	return result, nil
}

// atomicWrite writes content to target path atomically via write-to-tmp + rename.
func (w *Writer) atomicWrite(targetPath, content string) error {
	// Create parent directories
	dir := filepath.Dir(targetPath)
	if err := os.MkdirAll(dir, 0755); err != nil {
		return fmt.Errorf("cannot create directory: %w", err)
	}

	// Write to temp file
	tmpPath := filepath.Join(dir, "."+filepath.Base(targetPath)+".tmp")
	if err := os.WriteFile(tmpPath, []byte(content), 0644); err != nil {
		return fmt.Errorf("cannot write temp file: %w", err)
	}

	// Atomic rename
	if err := os.Rename(tmpPath, targetPath); err != nil {
		os.Remove(tmpPath) // cleanup
		return fmt.Errorf("cannot rename to target: %w", err)
	}

	return nil
}

// getMissionLock returns a per-mission-ID mutex to serialize WriteMission calls.
func (w *Writer) getMissionLock(id string) *sync.Mutex {
	mu, _ := w.missionLocks.LoadOrStore(id, &sync.Mutex{})
	return mu.(*sync.Mutex)
}
