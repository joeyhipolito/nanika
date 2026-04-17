package worker

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

// bootstrapIdentityTemplate is the initial identity.md template for a new persistent worker.
// The %s placeholder is filled with the worker name via fmt.Sprintf in bootstrap().
const bootstrapIdentityTemplate = "I am %s, a persistent worker. I accumulate memory and evolve my approach across missions.\n"

// identityStats is the on-disk representation of a worker's operational stats.
// The evals field is reserved for future eval mission results and is never written by
// the worker itself — the eval command will populate it.
type identityStats struct {
	PhasesCompleted int               `json:"phases_completed"`
	Domains         map[string]int    `json:"domains"`
	TotalCost       float64           `json:"total_cost"`
	LastActive      string            `json:"last_active"`
	CreatedAt       string            `json:"created_at,omitempty"`
	Evals           []json.RawMessage `json:"evals"`
}

// WorkerIdentity represents a persistent worker with accumulated memory and stats.
// The zero value is not usable; always obtain via LoadIdentity.
type WorkerIdentity struct {
	Name            string
	CreatedAt       time.Time
	PhasesCompleted int
	Domains         map[string]int
	TotalCost       float64
	LastActive      time.Time
	Entries         []*MemoryEntry

	dir string // absolute path to ~/.alluka/workers/<name>
}

// workerIdentityDir returns ~/.alluka/workers/<name>.
func workerIdentityDir(name string) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home dir: %w", err)
	}
	return filepath.Join(home, ".alluka", "workers", name), nil
}

// LoadIdentity reads a worker identity from ~/.alluka/workers/<name>/.
// If the directory does not exist, it bootstraps the worker with minimal files
// and returns the initialized identity.
func LoadIdentity(name string) (*WorkerIdentity, error) {
	dir, err := workerIdentityDir(name)
	if err != nil {
		return nil, fmt.Errorf("resolving worker identity dir: %w", err)
	}

	wi := &WorkerIdentity{
		Name:    name,
		Domains: make(map[string]int),
		dir:     dir,
	}

	if _, statErr := os.Stat(dir); os.IsNotExist(statErr) {
		if err := wi.bootstrap(); err != nil {
			return nil, fmt.Errorf("bootstrapping worker %q: %w", name, err)
		}
		return wi, nil
	}

	if err := wi.loadStats(); err != nil {
		return nil, fmt.Errorf("loading stats for worker %q: %w", name, err)
	}
	if err := wi.loadMemory(); err != nil {
		return nil, fmt.Errorf("loading memory for worker %q: %w", name, err)
	}
	return wi, nil
}

// bootstrap creates the worker directory and minimal initial files.
// Called only when the worker directory does not yet exist.
func (wi *WorkerIdentity) bootstrap() error {
	if err := os.MkdirAll(wi.dir, 0700); err != nil {
		return fmt.Errorf("creating worker dir: %w", err)
	}

	identityPath := filepath.Join(wi.dir, "identity.md")
	content := fmt.Sprintf(bootstrapIdentityTemplate, wi.Name)
	if err := os.WriteFile(identityPath, []byte(content), 0600); err != nil {
		return fmt.Errorf("writing identity.md: %w", err)
	}

	wi.CreatedAt = time.Now().UTC()

	if err := wi.saveStats(); err != nil {
		return fmt.Errorf("writing initial stats.json: %w", err)
	}

	memPath := filepath.Join(wi.dir, "memory.md")
	if err := os.WriteFile(memPath, []byte(""), 0600); err != nil {
		return fmt.Errorf("writing memory.md: %w", err)
	}

	return nil
}

// loadStats reads stats.json and populates the WorkerIdentity fields.
// A missing file is treated as empty stats (not an error).
func (wi *WorkerIdentity) loadStats() error {
	statsPath := filepath.Join(wi.dir, "stats.json")
	data, err := os.ReadFile(statsPath)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("reading stats.json: %w", err)
	}

	var s identityStats
	if err := json.Unmarshal(data, &s); err != nil {
		return fmt.Errorf("parsing stats.json: %w", err)
	}

	wi.PhasesCompleted = s.PhasesCompleted
	if s.Domains != nil {
		wi.Domains = s.Domains
	}
	wi.TotalCost = s.TotalCost
	if s.LastActive != "" {
		if t, err := time.Parse(time.RFC3339, s.LastActive); err == nil {
			wi.LastActive = t
		}
	}
	if s.CreatedAt != "" {
		if t, err := time.Parse(time.RFC3339, s.CreatedAt); err == nil {
			wi.CreatedAt = t
		}
	}
	return nil
}

// loadMemory reads memory.md and populates Entries.
// A missing file is treated as empty memory (not an error).
// If the on-disk file exceeds workerMemoryCeiling, entries are trimmed on load
// so that oversized files converge to the ceiling.
func (wi *WorkerIdentity) loadMemory() error {
	memPath := filepath.Join(wi.dir, "memory.md")
	data, err := os.ReadFile(memPath)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("reading memory.md: %w", err)
	}

	for _, line := range strings.Split(string(data), "\n") {
		trimmed := strings.TrimSpace(line)
		if trimmed == "" {
			continue
		}
		if e := ParseMemoryEntry(trimmed); e != nil {
			wi.Entries = append(wi.Entries, e)
		}
	}

	// Trim oversized files on load so the ceiling converges.
	wi.evictLowestScoring()

	return nil
}

// SaveIdentity persists stats.json and memory.md to disk. Both writes are atomic
// (write to .tmp then rename) to prevent corruption on crash.
func (wi *WorkerIdentity) SaveIdentity() error {
	if err := os.MkdirAll(wi.dir, 0700); err != nil {
		return fmt.Errorf("ensuring worker dir: %w", err)
	}
	if err := wi.saveStats(); err != nil {
		return fmt.Errorf("saving stats: %w", err)
	}
	if err := wi.saveMemory(); err != nil {
		return fmt.Errorf("saving memory: %w", err)
	}
	return nil
}

// saveStats writes stats.json atomically.
func (wi *WorkerIdentity) saveStats() error {
	domains := wi.Domains
	if domains == nil {
		domains = make(map[string]int)
	}

	s := identityStats{
		PhasesCompleted: wi.PhasesCompleted,
		Domains:         domains,
		TotalCost:       wi.TotalCost,
		Evals:           []json.RawMessage{},
	}
	if !wi.LastActive.IsZero() {
		s.LastActive = wi.LastActive.UTC().Format(time.RFC3339)
	}
	if !wi.CreatedAt.IsZero() {
		s.CreatedAt = wi.CreatedAt.UTC().Format(time.RFC3339)
	}

	data, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling stats.json: %w", err)
	}

	return atomicWrite(filepath.Join(wi.dir, "stats.json"), data, 0600)
}

// saveMemory rewrites memory.md from the current Entries slice atomically.
func (wi *WorkerIdentity) saveMemory() error {
	var buf strings.Builder
	for _, e := range wi.Entries {
		buf.WriteString(e.String())
		buf.WriteByte('\n')
	}
	return atomicWrite(filepath.Join(wi.dir, "memory.md"), []byte(buf.String()), 0600)
}

// atomicWrite writes data to path via a temp file + rename to prevent partial writes.
func atomicWrite(path string, data []byte, mode os.FileMode) error {
	tmpPath := path + ".tmp"
	if err := os.WriteFile(tmpPath, data, mode); err != nil {
		return fmt.Errorf("writing temp file %s: %w", tmpPath, err)
	}
	if err := os.Rename(tmpPath, path); err != nil {
		_ = os.Remove(tmpPath)
		return fmt.Errorf("renaming %s to %s: %w", tmpPath, path, err)
	}
	return nil
}

// RecordPhase increments PhasesCompleted, the per-domain counter, TotalCost,
// and sets LastActive to now. An empty domain string is accepted and ignored.
func (wi *WorkerIdentity) RecordPhase(domain string, cost float64) {
	wi.PhasesCompleted++
	if wi.Domains == nil {
		wi.Domains = make(map[string]int)
	}
	if domain != "" {
		wi.Domains[domain]++
	}
	wi.TotalCost += cost
	wi.LastActive = time.Now().UTC()
}

// workerMemoryCeiling is the maximum number of entries a worker's Entries slice
// may hold. When adding a new entry would exceed this limit, the lowest-scoring
// active entry (by recency weight) is evicted.
const workerMemoryCeiling = 100

// AddMemoryEntry appends entry to worker memory, silently dropping exact duplicates.
// Deduplication is based on the normalized content hash (same algorithm as the global
// memory pipeline). Superseded entries are ignored when checking for duplicates.
// When the entry count reaches workerMemoryCeiling, the lowest-scoring active entry
// (by recency weight) is evicted to make room.
func (wi *WorkerIdentity) AddMemoryEntry(entry MemoryEntry) {
	incoming := &entry
	for _, existing := range wi.Entries {
		if existing.isSuperseded() {
			continue
		}
		if incoming.isDuplicateOf(existing) {
			return
		}
	}
	wi.Entries = append(wi.Entries, incoming)

	// Enforce ceiling: evict lowest-scoring active entry when over limit.
	if len(wi.Entries) > workerMemoryCeiling {
		wi.evictLowestScoring()
	}
}

// evictLowestScoring removes entries until len(Entries) <= workerMemoryCeiling.
// Superseded entries are evicted first (they're inactive). Among active entries
// the one with the lowest recency score is removed; ties are broken by position
// (earliest index removed first).
func (wi *WorkerIdentity) evictLowestScoring() {
	for len(wi.Entries) > workerMemoryCeiling {
		now := time.Now()
		worstIdx := -1
		worstScore := 2.0 // higher than any possible recencyWeight

		for i, e := range wi.Entries {
			// Prefer evicting superseded entries unconditionally.
			if e.isSuperseded() {
				wi.Entries = append(wi.Entries[:i], wi.Entries[i+1:]...)
				worstIdx = -2 // sentinel: already evicted one
				break
			}
			score := recencyWeight(e, now)
			if score < worstScore {
				worstScore = score
				worstIdx = i
			}
		}
		if worstIdx == -2 {
			continue // superseded entry was evicted, re-check length
		}
		if worstIdx >= 0 {
			wi.Entries = append(wi.Entries[:worstIdx], wi.Entries[worstIdx+1:]...)
		} else {
			break // safety: nothing to evict
		}
	}
}

// BudgetedMemory returns the highest-scoring active entries up to budget bytes.
// Scoring uses keyword overlap (Jaccard) × recency weight — the same algorithm as
// seedMemory. When no entry overlaps the keywords, scoring falls back to recency-only.
// The budget accounts for entry string length + 1 byte for the newline separator.
func (wi *WorkerIdentity) BudgetedMemory(keywords []string, budget int) []*MemoryEntry {
	if len(wi.Entries) == 0 || budget <= 0 {
		return nil
	}

	kwSet := make(map[string]struct{}, len(keywords))
	for _, kw := range keywords {
		kw = strings.TrimSpace(strings.ToLower(kw))
		if kw != "" {
			kwSet[kw] = struct{}{}
		}
	}

	now := time.Now()

	type rankedEntry struct {
		e     *MemoryEntry
		score float64
	}

	var ranked []rankedEntry
	anyOverlap := false
	for _, e := range wi.Entries {
		if e.isSuperseded() {
			continue
		}
		overlap := keywordOverlapScore(e, kwSet)
		if overlap > 0 {
			anyOverlap = true
		}
		ranked = append(ranked, rankedEntry{e: e, score: overlap * recencyWeight(e, now)})
	}

	if !anyOverlap {
		for i := range ranked {
			ranked[i].score = recencyWeight(ranked[i].e, now)
		}
	}

	sort.SliceStable(ranked, func(i, j int) bool {
		return ranked[i].score > ranked[j].score
	})

	var result []*MemoryEntry
	used := 0
	for _, r := range ranked {
		line := r.e.String()
		lineBytes := len(line) + 1
		if used+lineBytes > budget {
			break
		}
		result = append(result, r.e)
		used += lineBytes
	}
	return result
}
