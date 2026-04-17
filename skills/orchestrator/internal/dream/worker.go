package dream

import (
	"bufio"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
	"github.com/joeyhipolito/orchestrator-cli/internal/worker"
)

// WorkerMemoryRef identifies a discovered worker memory file.
type WorkerMemoryRef struct {
	WorkerName  string
	Path        string
	ContentHash string // sha256 hex; populated before processing
}

// WorkerReport summarises one worker dream run.
type WorkerReport struct {
	StartedAt         time.Time
	Duration          time.Duration
	Discovered        int // worker memory files found
	SkippedFile       int // unchanged or filtered
	ProcessedFiles    int
	EntriesParsed     int
	EntriesSkipped    int // reflections, superseded, too short, already processed
	EntriesProcessed  int
	LearningsStored   int
	LearningsRejected int // failed Insert dedup (cosine gate)
	Errors            []RunError
}

// WorkerRunner orchestrates worker memory → learnings extraction.
// Worker memory entries are already structured learnings, so this pipeline
// does direct conversion with heuristic quality scoring instead of calling
// the LLM (CaptureFromConversation assumes raw human-Claude dialogue).
type WorkerRunner struct {
	store      Store
	learningDB *learning.DB
	embedder   *learning.Embedder
	cfg        Config
}

// NewWorkerRunner creates a WorkerRunner with the given components.
func NewWorkerRunner(store Store, ldb *learning.DB, embedder *learning.Embedder, cfg Config) *WorkerRunner {
	return &WorkerRunner{
		store:      store,
		learningDB: ldb,
		embedder:   embedder,
		cfg:        cfg,
	}
}

// Run executes one worker dream cycle. domain is passed through to learnings.
func (r *WorkerRunner) Run(ctx context.Context, domain string) (WorkerReport, error) {
	report := WorkerReport{StartedAt: time.Now()}

	refs, err := r.discoverWorkers()
	if err != nil {
		return report, fmt.Errorf("discover workers: %w", err)
	}
	report.Discovered = len(refs)

	for _, ref := range refs {
		if ctx.Err() != nil {
			break
		}
		if err := r.processMemory(ctx, ref, domain, &report); err != nil {
			report.Errors = append(report.Errors, RunError{
				Path:  ref.Path,
				Phase: "process",
				Err:   err.Error(),
			})
		}
	}

	report.Duration = time.Since(report.StartedAt)
	return report, nil
}

// discoverWorkers walks ~/.alluka/workers/ for memory.md files.
func (r *WorkerRunner) discoverWorkers() ([]WorkerMemoryRef, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, fmt.Errorf("user home dir: %w", err)
	}

	workersDir := filepath.Join(home, ".alluka", "workers")
	if _, err := os.Stat(workersDir); os.IsNotExist(err) {
		return nil, nil // no workers directory yet
	}

	entries, err := os.ReadDir(workersDir)
	if err != nil {
		return nil, fmt.Errorf("reading workers dir: %w", err)
	}

	var refs []WorkerMemoryRef
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}

		memPath := filepath.Join(workersDir, entry.Name(), "memory.md")
		if _, err := os.Stat(memPath); os.IsNotExist(err) {
			continue
		}

		// SessionFilter reused as worker name filter.
		if r.cfg.SessionFilter != "" && !strings.Contains(entry.Name(), r.cfg.SessionFilter) {
			continue
		}

		refs = append(refs, WorkerMemoryRef{
			WorkerName: entry.Name(),
			Path:       memPath,
		})
	}

	return refs, nil
}

// processMemory runs the full pipeline for one worker's memory.md.
// Read-only: never modifies the source memory.md file.
func (r *WorkerRunner) processMemory(ctx context.Context, ref WorkerMemoryRef, domain string, report *WorkerReport) error {
	// Layer 1 dedup: file-level content hash.
	hash, err := hashFile(ref.Path)
	if err != nil {
		report.SkippedFile++
		return fmt.Errorf("hashing %s: %w", ref.Path, err)
	}
	ref.ContentHash = hash

	if !r.cfg.Force {
		processed, err := r.store.IsFileProcessed(hash)
		if err != nil {
			return fmt.Errorf("checking file processed: %w", err)
		}
		if processed {
			report.SkippedFile++
			if r.cfg.Verbose {
				fmt.Printf("dream worker: skip %s (unchanged)\n", ref.WorkerName)
			}
			return nil
		}
	}

	// Parse memory entries.
	entries, err := parseMemoryFile(ref.Path)
	if err != nil {
		report.SkippedFile++
		report.Errors = append(report.Errors, RunError{Path: ref.Path, Phase: "parse", Err: err.Error()})
		return nil // non-fatal: skip the file
	}
	report.EntriesParsed += len(entries)

	// Filter to eligible entries.
	var eligible []*worker.MemoryEntry
	for _, entry := range entries {
		if !isEligibleEntry(entry) {
			report.EntriesSkipped++
			continue
		}
		eligible = append(eligible, entry)
	}

	report.ProcessedFiles++

	if r.cfg.Verbose {
		fmt.Printf("dream worker: %s — %d entries, %d eligible\n",
			ref.WorkerName, len(entries), len(eligible))
	}

	if r.cfg.DryRun {
		for _, entry := range eligible {
			fmt.Printf("  [%s] (score=%.2f) %s\n",
				entry.Type, scoreMemoryEntry(entry), truncateStr(entry.Content, 120))
		}
		return nil
	}

	// Convert and store each eligible entry.
	virtualPath := "worker://" + ref.WorkerName + "/memory.md"
	for i, entry := range eligible {
		if ctx.Err() != nil {
			break
		}

		entryHash := memoryEntryHash(entry.Content)

		// Layer 2 dedup: entry-level hash.
		isProcessed, err := r.store.IsChunkProcessed(entryHash)
		if err != nil {
			report.Errors = append(report.Errors, RunError{
				Path:  ref.Path,
				Phase: "dedup",
				Err:   fmt.Sprintf("entry %d: %s", i, err.Error()),
			})
			continue
		}
		if isProcessed {
			report.EntriesSkipped++
			continue
		}

		report.EntriesProcessed++

		l := convertEntryToLearning(entry, ref.WorkerName, domain)

		// Layer 3 dedup: cosine similarity inside DB.Insert.
		if err := r.learningDB.Insert(ctx, l, r.embedder); err != nil {
			report.LearningsRejected++
			if r.cfg.Verbose {
				fmt.Printf("  rejected: %s\n", truncateStr(entry.Content, 60))
			}
		} else {
			report.LearningsStored++
		}

		// Mark entry processed regardless of Insert outcome.
		if err := r.store.MarkChunkProcessed(virtualPath, entryHash, i); err != nil {
			report.Errors = append(report.Errors, RunError{
				Path:  ref.Path,
				Phase: "mark",
				Err:   err.Error(),
			})
		}
	}

	// Mark file processed after all entries.
	if err := r.store.MarkFileProcessed(virtualPath, hash, len(entries), len(eligible)); err != nil {
		return fmt.Errorf("marking file processed: %w", err)
	}

	return nil
}

// parseMemoryFile reads a memory.md file and returns parsed entries.
func parseMemoryFile(path string) ([]*worker.MemoryEntry, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, fmt.Errorf("opening %s: %w", path, err)
	}
	defer f.Close()

	var entries []*worker.MemoryEntry
	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		line := scanner.Text()
		if entry := worker.ParseMemoryEntry(line); entry != nil {
			entries = append(entries, entry)
		}
	}
	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("scanning %s: %w", path, err)
	}
	return entries, nil
}

// isEligibleEntry returns true if the entry should be processed for learnings.
func isEligibleEntry(entry *worker.MemoryEntry) bool {
	if entry == nil {
		return false
	}
	// Skip reflection entries (phase-completion stats).
	if entry.Type == "reflection" || strings.HasPrefix(entry.Content, "[reflection]") {
		return false
	}
	// Skip superseded entries (replaced by a correction).
	if entry.SupersededBy != "" {
		return false
	}
	// Skip entries too short to be useful learnings (matches isValidLearning threshold).
	if len(entry.Content) < 20 {
		return false
	}
	return true
}

// convertEntryToLearning maps a MemoryEntry to a Learning with heuristic quality scoring.
// No LLM call: worker memories are already structured learnings.
func convertEntryToLearning(entry *worker.MemoryEntry, workerName, domain string) learning.Learning {
	createdAt := entry.Filed
	if createdAt.IsZero() {
		createdAt = time.Now()
	}

	return learning.Learning{
		ID:           fmt.Sprintf("learn_%d", time.Now().UnixNano()),
		Type:         reverseMemoryType(entry.Type),
		Content:      entry.Content,
		Context:      "dream-worker:" + workerName,
		Domain:       domain,
		WorkerName:   workerName,
		QualityScore: scoreMemoryEntry(entry),
		CreatedAt:    createdAt,
	}
}

// reverseMemoryType maps memory entry types back to learning types.
// Inverse of worker.convertLearningType (promote.go:154).
func reverseMemoryType(memType string) learning.LearningType {
	switch memType {
	case "insight":
		return learning.TypeInsight
	case "pattern":
		return learning.TypePattern
	case "error":
		return learning.TypeError
	case "decision":
		return learning.TypeDecision
	case "reference":
		return learning.TypeSource
	case "feedback":
		return learning.TypeError
	case "user":
		return learning.TypeInsight
	default:
		return learning.TypeInsight
	}
}

// scoreMemoryEntry computes a heuristic quality score for a memory entry.
// Base score is 0.5 — higher than CaptureFromConversation's 0.4 because
// worker memories are already curated (they survived the memory pipeline).
func scoreMemoryEntry(entry *worker.MemoryEntry) float64 {
	score := 0.5

	// Content length: longer entries tend to be more detailed.
	n := len(entry.Content)
	if n > 200 {
		score += 0.15
	} else if n > 100 {
		score += 0.1
	} else if n > 50 {
		score += 0.05
	}

	// Metadata presence signals structured origin.
	if !entry.Filed.IsZero() {
		score += 0.05
	}
	if entry.Type != "" {
		score += 0.05
	}

	// Used count: entries that survived multiple sessions are validated.
	if entry.Used > 0 {
		score += 0.1 * math.Min(float64(entry.Used)/3.0, 1.0)
	}

	// Type bonus: decisions and patterns are high-value.
	switch entry.Type {
	case "decision", "pattern":
		score += 0.1
	case "error":
		score += 0.05
	}

	if score > 1.0 {
		score = 1.0
	}
	return score
}

// memoryEntryHash computes a SHA256 hash of normalized entry content.
// Mirrors worker.MemoryEntry.contentHash() logic: collapse whitespace,
// lowercase, strip leading bullet marker.
func memoryEntryHash(content string) string {
	normalized := strings.Join(strings.Fields(content), " ")
	normalized = strings.ToLower(normalized)
	normalized = strings.TrimPrefix(normalized, "- ")
	if normalized == "" {
		return ""
	}
	h := sha256.Sum256([]byte(normalized))
	return hex.EncodeToString(h[:])
}

// truncateStr shortens s to at most maxLen runes, appending "..." if truncated.
func truncateStr(s string, maxLen int) string {
	if len(s) <= maxLen {
		return s
	}
	return s[:maxLen-3] + "..."
}
