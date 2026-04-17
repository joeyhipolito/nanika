package worker

import (
	"bufio"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"time"
)

// metadataRe matches key: value pairs in metadata sections (compiled once).
var metadataRe = regexp.MustCompile(`(\w+):\s*([^|]+)`)

// memoryCeilingLines is the maximum number of non-empty entries in the canonical MEMORY.md.
// Entries beyond this limit are moved to MEMORY_ARCHIVE.md (oldest first).
const memoryCeilingLines = 100

// imperativePatterns are compiled once at package level.
// Any match quarantines the entry to MEMORY_QUARANTINE.md to prevent
// prompt-injection attacks disguised as memory learnings.
var imperativePatterns = []*regexp.Regexp{
	regexp.MustCompile(`(?i)\bignore\b.{0,60}\b(instructions?|rules?|previous|above|constraints?|guidelines?)\b`),
	regexp.MustCompile(`(?i)\b(disregard|bypass|dismiss)\b.{0,60}\b(instructions?|rules?|guidelines?|constraints?)\b`),
	regexp.MustCompile(`(?i)\bfrom now on\b`),
	regexp.MustCompile(`(?i)\byou are now\b`),
	regexp.MustCompile(`(?i)\bpretend (?:you are|to be)\b`),
	regexp.MustCompile(`(?i)\bsystem prompt\b`),
	regexp.MustCompile(`(?i)\b(?:reveal|print|output|display)\b.{0,40}\b(?:prompt|instructions?|system)\b`),
	regexp.MustCompile(`(?i)^\s*-?\s*\[(?:system|user|assistant)\]\s*:`),
	regexp.MustCompile(`(?i)\bnew instructions?\b`),
	regexp.MustCompile(`(?i)\byour (?:instructions?|system|rules?|constraints?)\b`),
	regexp.MustCompile(`(?i)\bdo not follow\b`),
	regexp.MustCompile(`(?i)\boverride (?:your|all|previous|the)\b`),
}

// correctionOverlapThreshold is the minimum Jaccard keyword similarity for
// two entries of the same type to be considered a correction (supersedure).
// The new entry must exceed this threshold (strictly greater than).
const correctionOverlapThreshold = 0.8

// MemoryEntry represents a single memory entry with optional inline metadata.
// Metadata fields are optional and backward-compatible with existing bare-text entries.
type MemoryEntry struct {
	Content      string    // The main text content of the memory
	Filed        time.Time // When the memory was created (zero if not set)
	By           string    // Persona or source that created it (empty if not set)
	Type         string    // Type of memory: user, feedback, project, reference (empty if not set)
	Used         int       // Count of how many times this memory has been used (0 if not set)
	SupersededBy string    // Content hash of the entry that replaced this one (empty = active)
	Bridged      time.Time // When the entry was bridged from a session MEMORY.md (zero if not bridged)
}

// ParseMemoryEntry parses a line into a MemoryEntry, extracting inline metadata if present.
// Format: "content text here | filed: 2026-04-09 | by: persona | type: user | used: 5"
// Handles backward compatibility with bare-text entries (no metadata).
// Whitespace around the content and metadata is trimmed.
func ParseMemoryEntry(line string) *MemoryEntry {
	line = strings.TrimSpace(line)
	if line == "" {
		return nil
	}

	entry := &MemoryEntry{}
	parts := strings.Split(line, "|")
	if len(parts) == 0 {
		return nil
	}

	entry.Content = strings.TrimSpace(parts[0])

	for i := 1; i < len(parts); i++ {
		matches := metadataRe.FindAllStringSubmatch(parts[i], -1)
		for _, match := range matches {
			key := strings.TrimSpace(match[1])
			val := strings.TrimSpace(match[2])

			switch key {
			case "filed":
				if t, err := time.Parse("2006-01-02", val); err == nil {
					entry.Filed = t
				}
			case "by":
				entry.By = val
			case "type":
				entry.Type = val
			case "used":
				if _, err := fmt.Sscanf(val, "%d", &entry.Used); err != nil {
					entry.Used = 0
				}
			case "superseded_by":
				entry.SupersededBy = val
			case "bridged":
				if t, err := time.Parse("2006-01-02", val); err == nil {
					entry.Bridged = t
				}
			}
		}
	}

	return entry
}

// String formats the MemoryEntry back into a line with inline metadata.
// If metadata fields are unset (zero values), they are omitted for cleanliness.
// Backward-compatible: returns just the content if no metadata is set.
func (e *MemoryEntry) String() string {
	if e == nil {
		return ""
	}

	result := e.Content
	var stamps []string
	if !e.Filed.IsZero() {
		stamps = append(stamps, fmt.Sprintf("filed: %s", e.Filed.Format("2006-01-02")))
	}
	if e.By != "" {
		stamps = append(stamps, fmt.Sprintf("by: %s", e.By))
	}
	if e.Type != "" {
		stamps = append(stamps, fmt.Sprintf("type: %s", e.Type))
	}
	if e.Used > 0 {
		stamps = append(stamps, fmt.Sprintf("used: %d", e.Used))
	}
	if e.SupersededBy != "" {
		stamps = append(stamps, fmt.Sprintf("superseded_by: %s", e.SupersededBy))
	}
	if !e.Bridged.IsZero() {
		stamps = append(stamps, fmt.Sprintf("bridged: %s", e.Bridged.Format("2006-01-02")))
	}

	if len(stamps) > 0 {
		result += " | " + strings.Join(stamps, " | ")
	}

	return result
}

// normalizedContent returns a normalized form of the entry's content for dedup comparison.
// Normalizes whitespace (collapses runs, trims edges), strips leading bullet markers
// ("- " prefix from old-format entries), and lowercases. The bullet stripping handles
// hash migration: old entries stored as "- content" must match new entries stored as
// "content" when comparing for duplicates.
func (e *MemoryEntry) normalizedContent() string {
	if e == nil || e.Content == "" {
		return ""
	}
	normalized := strings.Join(strings.Fields(e.Content), " ")
	normalized = strings.ToLower(normalized)
	// Strip leading bullet marker from old-format entries so "- foo" matches "foo".
	normalized = strings.TrimPrefix(normalized, "- ")
	return normalized
}

// contentHash returns a SHA256 hash of the normalized content for dedup.
func (e *MemoryEntry) contentHash() string {
	if e == nil {
		return ""
	}
	norm := e.normalizedContent()
	if norm == "" {
		return ""
	}
	h := sha256.Sum256([]byte(norm))
	return hex.EncodeToString(h[:])
}

// isDuplicateOf checks if this entry is a normalized duplicate of another.
// Returns true if normalized contents match and are non-empty.
// Empty hashes are never considered duplicates (prevents false positives on blank content).
func (e *MemoryEntry) isDuplicateOf(other *MemoryEntry) bool {
	if e == nil || other == nil {
		return false
	}
	hash1 := e.contentHash()
	hash2 := other.contentHash()
	if hash1 == "" || hash2 == "" {
		return false
	}
	return hash1 == hash2
}

// isSuperseded returns true if this entry has been replaced by a newer correction.
func (e *MemoryEntry) isSuperseded() bool {
	return e != nil && e.SupersededBy != ""
}

// keywords returns the set of lowercase words in the entry's content, stripped
// of leading/trailing punctuation. Used for correction detection via Jaccard similarity.
func (e *MemoryEntry) keywords() map[string]struct{} {
	if e == nil || e.Content == "" {
		return nil
	}
	words := strings.Fields(strings.ToLower(e.Content))
	set := make(map[string]struct{}, len(words))
	for _, w := range words {
		w = strings.Trim(w, ".,;:!?\"'()-[]{}#*")
		if w != "" {
			set[w] = struct{}{}
		}
	}
	return set
}

// keywordOverlap computes the Jaccard similarity between two entries' keyword sets.
// Returns 0.0 if either entry has no extractable keywords.
func keywordOverlap(a, b *MemoryEntry) float64 {
	ka := a.keywords()
	kb := b.keywords()
	if len(ka) == 0 || len(kb) == 0 {
		return 0
	}
	intersection := 0
	for w := range ka {
		if _, ok := kb[w]; ok {
			intersection++
		}
	}
	union := len(ka) + len(kb) - intersection
	if union == 0 {
		return 0
	}
	return float64(intersection) / float64(union)
}

// encodeProjectKey converts an absolute directory path into the key Claude uses
// for its per-project auto-memory directory. Both '/' and '.' are replaced with '-'.
func encodeProjectKey(dir string) string {
	r := strings.NewReplacer("/", "-", ".", "-")
	return r.Replace(dir)
}

// canonicalMemoryPath returns ~/nanika/personas/<persona>/MEMORY.md.
func canonicalMemoryPath(personaName string) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home dir: %w", err)
	}
	return filepath.Join(home, "nanika", "personas", personaName, "MEMORY.md"), nil
}

// globalMemoryPath returns ~/.alluka/memory/global.md.
func globalMemoryPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home dir: %w", err)
	}
	return filepath.Join(home, ".alluka", "memory", "global.md"), nil
}

// workerMemoryPath returns ~/.claude/projects/<encoded-workerDir>/memory/MEMORY.md.
func workerMemoryPath(workerDir string) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home dir: %w", err)
	}
	key := encodeProjectKey(workerDir)
	return filepath.Join(home, ".claude", "projects", key, "memory", "MEMORY.md"), nil
}

// workerMemoryNewPath returns ~/.claude/projects/<encoded-workerDir>/memory/MEMORY_NEW.md.
func workerMemoryNewPath(workerDir string) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home dir: %w", err)
	}
	key := encodeProjectKey(workerDir)
	return filepath.Join(home, ".claude", "projects", key, "memory", "MEMORY_NEW.md"), nil
}

// quarantineMemoryPath returns ~/nanika/personas/<persona>/MEMORY_QUARANTINE.md.
func quarantineMemoryPath(personaName string) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home dir: %w", err)
	}
	return filepath.Join(home, "nanika", "personas", personaName, "MEMORY_QUARANTINE.md"), nil
}

// archiveMemoryPath returns ~/nanika/personas/<persona>/MEMORY_ARCHIVE.md.
func archiveMemoryPath(personaName string) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home dir: %w", err)
	}
	return filepath.Join(home, "nanika", "personas", personaName, "MEMORY_ARCHIVE.md"), nil
}

// containsInvisibleUnicode reports whether s contains any invisible Unicode:
// zero-width spaces (U+200B–U+200F) or directional overrides (U+202A–U+202E).
func containsInvisibleUnicode(s string) bool {
	for _, r := range s {
		if r >= 0x200B && r <= 0x200F {
			return true
		}
		if r >= 0x202A && r <= 0x202E {
			return true
		}
	}
	return false
}

// appendToQuarantine writes line with a quarantine reason comment to MEMORY_QUARANTINE.md.
func appendToQuarantine(personaName, line, reason string) error {
	path, err := quarantineMemoryPath(personaName)
	if err != nil {
		return fmt.Errorf("resolving quarantine path: %w", err)
	}
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return fmt.Errorf("creating quarantine dir: %w", err)
	}
	f, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0600)
	if err != nil {
		return fmt.Errorf("opening quarantine file: %w", err)
	}
	defer f.Close()
	if _, err := fmt.Fprintf(f, "%s <!-- quarantined: %s -->\n", line, reason); err != nil {
		return fmt.Errorf("writing quarantine entry: %w", err)
	}
	return nil
}

// safetyGate checks a line for imperative patterns and invisible Unicode.
// Returns false and appends the line to MEMORY_QUARANTINE.md when unsafe.
// Safe lines pass through unmodified.
func safetyGate(personaName, line string) (bool, error) {
	if containsInvisibleUnicode(line) {
		if err := appendToQuarantine(personaName, line, "invisible unicode"); err != nil {
			return false, fmt.Errorf("quarantining invisible unicode entry: %w", err)
		}
		return false, nil
	}
	for _, pat := range imperativePatterns {
		if pat.MatchString(line) {
			if err := appendToQuarantine(personaName, line, "imperative pattern"); err != nil {
				return false, fmt.Errorf("quarantining imperative pattern entry: %w", err)
			}
			return false, nil
		}
	}
	return true, nil
}

// enforceMemoryCeiling caps the canonical MEMORY.md at memoryCeilingLines non-empty lines.
// When the file exceeds the cap, the oldest lines (from the top) are moved to
// MEMORY_ARCHIVE.md so the canonical file retains only the most recent entries.
func enforceMemoryCeiling(personaName string) error {
	canonical, err := canonicalMemoryPath(personaName)
	if err != nil {
		return fmt.Errorf("resolving canonical path: %w", err)
	}

	content, err := os.ReadFile(canonical)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("reading canonical MEMORY.md: %w", err)
	}

	var lines []string
	scanner := bufio.NewScanner(strings.NewReader(string(content)))
	for scanner.Scan() {
		line := scanner.Text()
		if strings.TrimSpace(line) != "" {
			lines = append(lines, line)
		}
	}

	if len(lines) <= memoryCeilingLines {
		return nil
	}

	archivePath, err := archiveMemoryPath(personaName)
	if err != nil {
		return fmt.Errorf("resolving archive path: %w", err)
	}
	if err := os.MkdirAll(filepath.Dir(archivePath), 0700); err != nil {
		return fmt.Errorf("creating archive dir: %w", err)
	}

	excess := lines[:len(lines)-memoryCeilingLines]
	keep := lines[len(lines)-memoryCeilingLines:]

	af, err := os.OpenFile(archivePath, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0600)
	if err != nil {
		return fmt.Errorf("opening archive file: %w", err)
	}
	for _, line := range excess {
		if _, err := fmt.Fprintln(af, line); err != nil {
			af.Close()
			return fmt.Errorf("archiving line: %w", err)
		}
	}
	if err := af.Close(); err != nil {
		return fmt.Errorf("closing archive file: %w", err)
	}

	var buf strings.Builder
	for _, line := range keep {
		buf.WriteString(line)
		buf.WriteByte('\n')
	}
	if err := os.WriteFile(canonical, []byte(buf.String()), 0600); err != nil {
		return fmt.Errorf("rewriting canonical MEMORY.md after ceiling: %w", err)
	}

	return nil
}

// promoteEntriesToGlobal appends entries to ~/.alluka/memory/global.md, deduplicating
// against existing global content. Returns the number of entries actually written.
func promoteEntriesToGlobal(entries []*MemoryEntry) (int, error) {
	if len(entries) == 0 {
		return 0, nil
	}
	globalPath, err := globalMemoryPath()
	if err != nil {
		return 0, fmt.Errorf("resolving global memory path: %w", err)
	}
	if err := os.MkdirAll(filepath.Dir(globalPath), 0700); err != nil {
		return 0, fmt.Errorf("creating global memory dir: %w", err)
	}

	// Load existing global entries for dedup.
	// Two maps: hash-based (original) and normalized-content-based (handles
	// migration from old body-hashed entries to new name-hashed entries).
	existingHashes := make(map[string]bool)
	existingNorm := make(map[string]bool)
	if globalContent, rerr := os.ReadFile(globalPath); rerr == nil {
		for _, line := range strings.Split(string(globalContent), "\n") {
			trimmed := strings.TrimSpace(line)
			if trimmed == "" {
				continue
			}
			if e := ParseMemoryEntry(trimmed); e != nil {
				if h := e.contentHash(); h != "" {
					existingHashes[h] = true
				}
				if nc := e.normalizedContent(); nc != "" {
					existingNorm[nc] = true
				}
			}
		}
	}

	f, err := os.OpenFile(globalPath, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0600)
	if err != nil {
		return 0, fmt.Errorf("opening global MEMORY.md for append: %w", err)
	}
	promoted := 0
	for _, e := range entries {
		h := e.contentHash()
		nc := e.normalizedContent()
		// Deduplicate: either hash match (same content, same normalization) or
		// normalized content match (handles old-style body hash vs new-style name hash).
		if (h != "" && existingHashes[h]) || (nc != "" && existingNorm[nc]) {
			continue
		}
		if _, werr := fmt.Fprintln(f, e.String()); werr != nil {
			f.Close()
			return promoted, fmt.Errorf("writing to global MEMORY.md: %w", werr)
		}
		if h != "" {
			existingHashes[h] = true
		}
		if nc != "" {
			existingNorm[nc] = true
		}
		promoted++
	}
	if err := f.Close(); err != nil {
		return promoted, fmt.Errorf("closing global MEMORY.md: %w", err)
	}
	return promoted, nil
}

// seedMemoryBudgetBytes is the maximum byte size of entries written to the worker
// MEMORY.md. Top-scored entries are selected until the budget is exhausted.
const seedMemoryBudgetBytes = 4 * 1024

// objectiveKeywords extracts the lowercase word set from an objective string,
// using the same punctuation stripping as MemoryEntry.keywords().
func objectiveKeywords(objective string) map[string]struct{} {
	if objective == "" {
		return nil
	}
	words := strings.Fields(strings.ToLower(objective))
	set := make(map[string]struct{}, len(words))
	for _, w := range words {
		w = strings.Trim(w, ".,;:!?\"'()-[]{}#*")
		if w != "" {
			set[w] = struct{}{}
		}
	}
	return set
}

// recencyWeight returns exp(-0.02 * days) where days is the age of the entry
// since its Filed date. Half-life is ~35 days. If Filed is zero, returns 1.0.
func recencyWeight(entry *MemoryEntry, now time.Time) float64 {
	if entry == nil || entry.Filed.IsZero() {
		return 1.0
	}
	days := now.Sub(entry.Filed).Hours() / 24
	if days < 0 {
		days = 0
	}
	return math.Exp(-0.02 * days)
}

// keywordOverlapScore computes the Jaccard similarity between an entry's keyword
// set and the given objective keyword set. Returns 0.0 if either set is empty.
func keywordOverlapScore(entry *MemoryEntry, objectiveKWs map[string]struct{}) float64 {
	if len(objectiveKWs) == 0 {
		return 0
	}
	entryKWs := entry.keywords()
	if len(entryKWs) == 0 {
		return 0
	}
	intersection := 0
	for w := range objectiveKWs {
		if _, ok := entryKWs[w]; ok {
			intersection++
		}
	}
	union := len(objectiveKWs) + len(entryKWs) - intersection
	if union == 0 {
		return 0
	}
	return float64(intersection) / float64(union)
}

// scoreEntry computes the relevance score for an entry against an objective.
// Score = keyword overlap (Jaccard) * recency weight (exp(-0.02*days)).
// Used by seedMemory to rank entries before budget selection.
func scoreEntry(entry *MemoryEntry, objectiveKWs map[string]struct{}, now time.Time) float64 {
	return keywordOverlapScore(entry, objectiveKWs) * recencyWeight(entry, now)
}

// loadScoredEntries reads entries from a MEMORY.md file, filters out superseded
// entries, and returns them ranked by keyword overlap * recency. Falls back to
// recency-only when no entry matches the objective keywords. Returns nil when the
// file does not exist (not an error — caller treats it as empty).
func loadScoredEntries(path string, objKWs map[string]struct{}, now time.Time) ([]string, error) {
	content, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("reading %s: %w", path, err)
	}

	type activeEntry struct {
		line  string
		entry *MemoryEntry
	}
	var active []activeEntry
	for _, line := range strings.Split(string(content), "\n") {
		trimmed := strings.TrimSpace(line)
		if trimmed == "" {
			continue
		}
		e := ParseMemoryEntry(trimmed)
		if e != nil && e.isSuperseded() {
			continue
		}
		active = append(active, activeEntry{line: trimmed, entry: e})
	}

	type rankedEntry struct {
		line  string
		score float64
	}
	ranked := make([]rankedEntry, len(active))
	anyOverlap := false
	for i, ae := range active {
		overlap := keywordOverlapScore(ae.entry, objKWs)
		if overlap > 0 {
			anyOverlap = true
		}
		ranked[i] = rankedEntry{line: ae.line, score: overlap * recencyWeight(ae.entry, now)}
	}
	if !anyOverlap {
		for i, ae := range active {
			ranked[i] = rankedEntry{line: ae.line, score: recencyWeight(ae.entry, now)}
		}
	}
	sort.SliceStable(ranked, func(i, j int) bool {
		return ranked[i].score > ranked[j].score
	})

	out := make([]string, len(ranked))
	for i, r := range ranked {
		out[i] = r.line
	}
	return out, nil
}

// seedMemory copies the canonical persona MEMORY.md (and the global MEMORY.md) into
// the worker's Claude auto-memory location so the spawned session inherits prior
// learnings. Global entries are prepended before persona entries; both draw from the
// same 4KB budget. Persona entries selected for seeding have their Used counter
// incremented in the canonical file. When no keywords match the objective, scoring
// falls back to recency-only.
func seedMemory(personaName, workerDir, objective string) error {
	canonical, err := canonicalMemoryPath(personaName)
	if err != nil {
		return fmt.Errorf("resolving canonical path: %w", err)
	}

	// Ensure canonical file exists (create empty if absent).
	if _, statErr := os.Stat(canonical); os.IsNotExist(statErr) {
		if err := os.MkdirAll(filepath.Dir(canonical), 0700); err != nil {
			return fmt.Errorf("creating persona dir: %w", err)
		}
		if err := os.WriteFile(canonical, []byte(""), 0600); err != nil {
			return fmt.Errorf("creating canonical MEMORY.md: %w", err)
		}
	}

	objKWs := objectiveKeywords(objective)
	now := time.Now()

	// Load global entries (prepended first, consume budget before persona entries).
	globalPath, err := globalMemoryPath()
	if err != nil {
		return fmt.Errorf("resolving global memory path: %w", err)
	}
	globalRanked, err := loadScoredEntries(globalPath, objKWs, now)
	if err != nil {
		return fmt.Errorf("loading global memory: %w", err)
	}

	// Load persona entries.
	personaRanked, err := loadScoredEntries(canonical, objKWs, now)
	if err != nil {
		return fmt.Errorf("loading persona memory: %w", err)
	}

	// Greedily fill the budget: global first, then persona.
	budget := 0
	var selectedGlobal, selectedPersona []string
	for _, line := range globalRanked {
		lineBytes := len(line) + 1
		if budget+lineBytes > seedMemoryBudgetBytes {
			break
		}
		selectedGlobal = append(selectedGlobal, line)
		budget += lineBytes
	}
	for _, line := range personaRanked {
		lineBytes := len(line) + 1
		if budget+lineBytes > seedMemoryBudgetBytes {
			break
		}
		selectedPersona = append(selectedPersona, line)
		budget += lineBytes
	}

	// Increment Used for each persona entry selected for seeding.
	if len(selectedPersona) > 0 {
		if err := incrementUsedInCanonical(canonical, selectedPersona); err != nil {
			// Non-fatal: continue seeding even if the increment fails.
			_ = err
		}
	}

	// Combine: global entries first, then persona entries.
	all := append(selectedGlobal, selectedPersona...)
	var filteredContent []byte
	if len(all) > 0 {
		filteredContent = []byte(strings.Join(all, "\n") + "\n")
	}

	dst, err := workerMemoryPath(workerDir)
	if err != nil {
		return fmt.Errorf("resolving worker memory path: %w", err)
	}

	if err := os.MkdirAll(filepath.Dir(dst), 0700); err != nil {
		return fmt.Errorf("creating worker memory dir: %w", err)
	}
	if err := os.WriteFile(dst, filteredContent, 0600); err != nil {
		return fmt.Errorf("writing worker MEMORY.md: %w", err)
	}
	// Make MEMORY.md read-only so the worker cannot overwrite the seeded snapshot.
	if err := os.Chmod(dst, 0400); err != nil {
		return fmt.Errorf("chmod worker MEMORY.md: %w", err)
	}

	// Create MEMORY_NEW.md as the writable scratchpad for new memories.
	newPath, err := workerMemoryNewPath(workerDir)
	if err != nil {
		return fmt.Errorf("resolving worker memory new path: %w", err)
	}
	if err := os.WriteFile(newPath, []byte(""), 0600); err != nil {
		return fmt.Errorf("creating worker MEMORY_NEW.md: %w", err)
	}
	return nil
}

// incrementUsedInCanonical rewrites the canonical MEMORY.md file incrementing the
// Used counter for each line whose trimmed text appears in the selectedLines slice.
// Lines not in selectedLines are written unchanged. The file mode is preserved.
func incrementUsedInCanonical(canonicalPath string, selectedLines []string) error {
	content, err := os.ReadFile(canonicalPath)
	if err != nil {
		return fmt.Errorf("reading canonical for used-increment: %w", err)
	}

	// Build a set of selected line texts for O(1) lookup.
	sel := make(map[string]bool, len(selectedLines))
	for _, l := range selectedLines {
		sel[strings.TrimSpace(l)] = true
	}

	var buf strings.Builder
	scanner := bufio.NewScanner(strings.NewReader(string(content)))
	for scanner.Scan() {
		line := scanner.Text()
		trimmed := strings.TrimSpace(line)
		if trimmed != "" && sel[trimmed] {
			e := ParseMemoryEntry(trimmed)
			if e != nil {
				e.Used++
				buf.WriteString(e.String())
				buf.WriteByte('\n')
				continue
			}
		}
		buf.WriteString(line)
		buf.WriteByte('\n')
	}
	if err := scanner.Err(); err != nil {
		return fmt.Errorf("scanning canonical for used-increment: %w", err)
	}
	return os.WriteFile(canonicalPath, []byte(buf.String()), 0600)
}

// usedPromotionThreshold is the minimum Used count for an entry to be automatically
// promoted from a persona's MEMORY.md to the global MEMORY.md during mergeMemoryBack.
const usedPromotionThreshold = 3

// autoPromoteHighUsed scans the persona canonical MEMORY.md for non-superseded entries
// with Used >= usedPromotionThreshold, promotes them to the global MEMORY.md, then
// rewrites the persona canonical without those entries.
func autoPromoteHighUsed(personaName string) error {
	canonical, err := canonicalMemoryPath(personaName)
	if err != nil {
		return fmt.Errorf("resolving canonical path: %w", err)
	}
	content, err := os.ReadFile(canonical)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("reading canonical for promotion: %w", err)
	}

	var toPromote []*MemoryEntry
	var keepLines []string
	scanner := bufio.NewScanner(strings.NewReader(string(content)))
	for scanner.Scan() {
		line := scanner.Text()
		trimmed := strings.TrimSpace(line)
		if trimmed == "" {
			continue
		}
		e := ParseMemoryEntry(trimmed)
		if e != nil && !e.isSuperseded() && e.Used >= usedPromotionThreshold {
			toPromote = append(toPromote, e)
		} else {
			keepLines = append(keepLines, line)
		}
	}
	if err := scanner.Err(); err != nil {
		return fmt.Errorf("scanning canonical for promotion: %w", err)
	}

	if len(toPromote) == 0 {
		return nil
	}

	if _, err := promoteEntriesToGlobal(toPromote); err != nil {
		return fmt.Errorf("promoting entries to global: %w", err)
	}

	// Rewrite canonical without promoted entries.
	var buf strings.Builder
	for _, l := range keepLines {
		buf.WriteString(l)
		buf.WriteByte('\n')
	}
	if err := os.WriteFile(canonical, []byte(buf.String()), 0600); err != nil {
		return fmt.Errorf("rewriting canonical after promotion: %w", err)
	}
	return nil
}

// PromotePersonaEntries reads the persona canonical MEMORY.md, selects non-superseded
// entries for which matcher returns true, promotes them to the global MEMORY.md, and
// removes them from the persona canonical. Returns the count of promoted entries.
// If matcher is nil, all non-superseded entries are candidates.
func PromotePersonaEntries(personaName string, matcher func(*MemoryEntry) bool) (int, error) {
	canonical, err := canonicalMemoryPath(personaName)
	if err != nil {
		return 0, fmt.Errorf("resolving canonical path: %w", err)
	}
	content, err := os.ReadFile(canonical)
	if err != nil {
		if os.IsNotExist(err) {
			return 0, nil
		}
		return 0, fmt.Errorf("reading persona canonical: %w", err)
	}

	var toPromote []*MemoryEntry
	var keepLines []string
	scanner := bufio.NewScanner(strings.NewReader(string(content)))
	for scanner.Scan() {
		line := scanner.Text()
		trimmed := strings.TrimSpace(line)
		if trimmed == "" {
			continue
		}
		e := ParseMemoryEntry(trimmed)
		if e != nil && !e.isSuperseded() && (matcher == nil || matcher(e)) {
			toPromote = append(toPromote, e)
		} else {
			keepLines = append(keepLines, line)
		}
	}
	if err := scanner.Err(); err != nil {
		return 0, fmt.Errorf("scanning persona canonical: %w", err)
	}

	if len(toPromote) == 0 {
		return 0, nil
	}

	promoted, err := promoteEntriesToGlobal(toPromote)
	if err != nil {
		return 0, fmt.Errorf("writing to global MEMORY.md: %w", err)
	}

	var buf strings.Builder
	for _, l := range keepLines {
		buf.WriteString(l)
		buf.WriteByte('\n')
	}
	if err := os.WriteFile(canonical, []byte(buf.String()), 0600); err != nil {
		return promoted, fmt.Errorf("rewriting persona canonical after promotion: %w", err)
	}
	return promoted, nil
}

// mergeMemoryBack reads the worker's post-session MEMORY_NEW.md scratchpad and
// appends any lines not present in the canonical file. Falls back to diffing
// against MEMORY.md when MEMORY_NEW.md is absent. Dedup uses normalized content
// comparison to catch semantically identical entries even with different formatting.
// This preserves memories the worker accumulated during execution without duplicating.
func mergeMemoryBack(personaName, workerDir string) error {
	newPath, err := workerMemoryNewPath(workerDir)
	if err != nil {
		return fmt.Errorf("resolving worker memory new path: %w", err)
	}

	var workerContent []byte
	workerContent, err = os.ReadFile(newPath)
	if err != nil {
		if !os.IsNotExist(err) {
			return fmt.Errorf("reading worker MEMORY_NEW.md: %w", err)
		}
		// MEMORY_NEW.md absent — fall back to diff against MEMORY.md.
		workerPath, ferr := workerMemoryPath(workerDir)
		if ferr != nil {
			return fmt.Errorf("resolving worker memory path: %w", ferr)
		}
		workerContent, err = os.ReadFile(workerPath)
		if err != nil {
			if os.IsNotExist(err) {
				return nil // Worker didn't create any memories.
			}
			return fmt.Errorf("reading worker MEMORY.md: %w", err)
		}
	}

	canonical, err := canonicalMemoryPath(personaName)
	if err != nil {
		return fmt.Errorf("resolving canonical path: %w", err)
	}

	canonicalContent, err := os.ReadFile(canonical)
	if err != nil {
		if os.IsNotExist(err) {
			canonicalContent = []byte{}
		} else {
			return fmt.Errorf("reading canonical MEMORY.md: %w", err)
		}
	}

	// Parse canonical into tracked entries for correction detection.
	type trackedEntry struct {
		raw   string
		entry *MemoryEntry
	}
	var canonEntries []trackedEntry
	scanner := bufio.NewScanner(strings.NewReader(string(canonicalContent)))
	for scanner.Scan() {
		line := scanner.Text()
		trimmed := strings.TrimSpace(line)
		var entry *MemoryEntry
		if trimmed != "" {
			entry = ParseMemoryEntry(trimmed)
		}
		canonEntries = append(canonEntries, trackedEntry{raw: line, entry: entry})
	}

	// Build hash set for efficient dedup and pre-compute keyword sets for correction detection.
	existingByHash := make(map[string]bool)
	canonKW := make([]map[string]struct{}, len(canonEntries))
	for i, ce := range canonEntries {
		if ce.entry != nil {
			if h := ce.entry.contentHash(); h != "" {
				existingByHash[h] = true
			}
			if ce.entry.Type != "" {
				canonKW[i] = ce.entry.keywords()
			}
		}
	}

	// Process worker entries: dedup, safety gate, correction detection.
	var newEntries []*MemoryEntry
	anySuperseded := false
	wScanner := bufio.NewScanner(strings.NewReader(string(workerContent)))
	for wScanner.Scan() {
		line := wScanner.Text()
		trimmed := strings.TrimSpace(line)
		if trimmed == "" {
			continue
		}

		// Safety gate: quarantine unsafe entries before any other processing.
		safe, sErr := safetyGate(personaName, trimmed)
		if sErr != nil {
			return fmt.Errorf("safety gate check: %w", sErr)
		}
		if !safe {
			continue
		}

		entry := ParseMemoryEntry(trimmed)
		if entry == nil {
			continue
		}

		hash := entry.contentHash()
		if hash != "" && existingByHash[hash] {
			continue // exact or normalized duplicate
		}

		// Correction detection: same non-empty type + keyword overlap > threshold
		// marks the old entry as superseded. Both entries persist for audit.
		if entry.Type != "" {
			newKW := entry.keywords()
			for i := range canonEntries {
				ce := &canonEntries[i]
				if ce.entry == nil || ce.entry.isSuperseded() || ce.entry.Type != entry.Type {
					continue
				}
				ckw := canonKW[i]
				if len(ckw) == 0 || len(newKW) == 0 {
					continue
				}
				intersection := 0
				for w := range ckw {
					if _, ok := newKW[w]; ok {
						intersection++
					}
				}
				union := len(ckw) + len(newKW) - intersection
				if union > 0 && float64(intersection)/float64(union) > correctionOverlapThreshold {
					ce.entry.SupersededBy = hash
					anySuperseded = true
				}
			}
		}

		if hash != "" {
			existingByHash[hash] = true
		}
		newEntries = append(newEntries, entry)
	}

	if anySuperseded {
		// Rewrite canonical with superseded marks and new entries appended.
		var buf strings.Builder
		for _, ce := range canonEntries {
			if ce.entry != nil && ce.entry.isSuperseded() {
				buf.WriteString(ce.entry.String())
			} else {
				buf.WriteString(ce.raw)
			}
			buf.WriteByte('\n')
		}
		for _, e := range newEntries {
			buf.WriteString(e.String())
			buf.WriteByte('\n')
		}
		if err := os.WriteFile(canonical, []byte(buf.String()), 0600); err != nil {
			return fmt.Errorf("writing canonical MEMORY.md: %w", err)
		}
	} else if len(newEntries) > 0 {
		// Append only (no supersedures detected).
		f, err := os.OpenFile(canonical, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0600)
		if err != nil {
			return fmt.Errorf("opening canonical MEMORY.md for append: %w", err)
		}

		if len(canonicalContent) > 0 && canonicalContent[len(canonicalContent)-1] != '\n' {
			if _, err := f.WriteString("\n"); err != nil {
				f.Close()
				return fmt.Errorf("writing newline: %w", err)
			}
		}

		for _, e := range newEntries {
			if _, err := fmt.Fprintln(f, e.String()); err != nil {
				f.Close()
				return fmt.Errorf("appending line: %w", err)
			}
		}
		if err := f.Close(); err != nil {
			return fmt.Errorf("closing canonical MEMORY.md: %w", err)
		}
	}

	// Enforce ceiling; archive oldest entries when canonical exceeds 100 lines.
	if err := enforceMemoryCeiling(personaName); err != nil {
		return fmt.Errorf("enforcing memory ceiling: %w", err)
	}

	// Auto-promote entries with used >= 3 to global MEMORY.md and remove from persona.
	if err := autoPromoteHighUsed(personaName); err != nil {
		return fmt.Errorf("auto-promoting high-used entries: %w", err)
	}

	// Restore MEMORY.md to writable for the next seed cycle.
	workerPath, err := workerMemoryPath(workerDir)
	if err != nil {
		return fmt.Errorf("resolving worker memory path for chmod: %w", err)
	}
	if err := os.Chmod(workerPath, 0600); err != nil && !os.IsNotExist(err) {
		return fmt.Errorf("restoring worker MEMORY.md permissions: %w", err)
	}

	// Remove the scratchpad so stale entries don't bleed into the next session.
	if err := os.Remove(newPath); err != nil && !os.IsNotExist(err) {
		return fmt.Errorf("removing worker MEMORY_NEW.md: %w", err)
	}

	return nil
}

// BridgeSessionMemory reads Claude Code's auto-memory directory for sourceDir,
// extracts entries with type "project" or "reference" from their YAML frontmatter,
// stamps each with a bridged: date, and merges them into ~/.alluka/memory/global.md.
// Idempotent: dedup is performed by content hash so re-running never duplicates.
// sourceDir is the project directory whose Claude auto-memory to read.
// When empty it defaults to ~/nanika (the main orchestrator project).
// Returns the number of entries newly written to global MEMORY.md.
func BridgeSessionMemory(sourceDir string) (int, error) {
	if sourceDir == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return 0, fmt.Errorf("getting home dir: %w", err)
		}
		sourceDir = filepath.Join(home, "nanika")
	}

	// Claude Code auto-memory lives in ~/.claude/projects/<key>/memory/ as
	// individual .md files with YAML frontmatter (name, description, type).
	// MEMORY.md in that directory is just an index — the real content is in
	// the individual files.
	srcPath, err := workerMemoryPath(sourceDir)
	if err != nil {
		return 0, fmt.Errorf("resolving session memory path: %w", err)
	}
	memDir := filepath.Dir(srcPath)

	entries, err := os.ReadDir(memDir)
	if err != nil {
		if os.IsNotExist(err) {
			return 0, nil // no session memory dir yet — not an error
		}
		return 0, fmt.Errorf("reading session memory dir %s: %w", memDir, err)
	}

	now := time.Now()
	var candidates []*MemoryEntry
	for _, de := range entries {
		if de.IsDir() || !strings.HasSuffix(de.Name(), ".md") {
			continue
		}
		// Skip MEMORY.md index file
		if strings.EqualFold(de.Name(), "memory.md") {
			continue
		}
		filePath := filepath.Join(memDir, de.Name())
		content, rerr := os.ReadFile(filePath)
		if rerr != nil {
			continue
		}
		entryType, name, body := parseClaudeMemoryFile(string(content))
		if entryType != "project" && entryType != "reference" {
			continue
		}
		// Use the name as content. If no name, use the first non-empty
		// line of body. Multi-line bodies don't round-trip through the
		// line-based global MEMORY.md format, so we only keep the summary.
		entryContent := name
		if entryContent == "" && body != "" {
			for _, line := range strings.Split(body, "\n") {
				line = strings.TrimSpace(line)
				if line != "" && !strings.HasPrefix(line, "#") {
					entryContent = line
					break
				}
			}
		}
		if entryContent == "" {
			continue
		}
		e := &MemoryEntry{
			Type:    entryType,
			Content: entryContent,
			Filed:   now,
			By:      "bridge",
			Bridged: now,
		}
		candidates = append(candidates, e)
	}

	return promoteEntriesToGlobal(candidates)
}

// parseClaudeMemoryFile extracts type, name, and body from a Claude Code
// auto-memory file that uses YAML frontmatter:
//
//	---
//	name: Some Name
//	description: ...
//	type: project
//	---
//	Body content here.
func parseClaudeMemoryFile(content string) (entryType, name, body string) {
	content = strings.TrimSpace(content)
	if !strings.HasPrefix(content, "---") {
		return "", "", content // no frontmatter — return full content as body
	}
	// Find closing ---
	rest := content[3:]
	idx := strings.Index(rest, "\n---")
	if idx < 0 {
		return "", "", content
	}
	frontmatter := rest[:idx]
	body = strings.TrimSpace(rest[idx+4:])

	for _, line := range strings.Split(frontmatter, "\n") {
		line = strings.TrimSpace(line)
		if k, v, ok := strings.Cut(line, ":"); ok {
			k = strings.TrimSpace(k)
			v = strings.TrimSpace(v)
			switch k {
			case "type":
				entryType = v
			case "name":
				name = v
			}
		}
	}
	return entryType, name, body
}
