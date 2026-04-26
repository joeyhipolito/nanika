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

// maxBridgedBytes caps the body content bridged from a session memory file to
// prevent runaway entries from flooding the global memory store.
const maxBridgedBytes = 1024

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

// seedMemory writes scored global memory entries into the worker's Claude auto-memory
// location so the spawned session inherits prior learnings. Entries are ranked by
// keyword overlap * recency and greedily selected up to seedMemoryBudgetBytes.
// When no keywords match the objective, scoring falls back to recency-only.
func seedMemory(personaName, workerDir, effectiveCWD, objective string) error {
	objKWs := objectiveKeywords(objective)
	now := time.Now()

	globalPath, err := globalMemoryPath()
	if err != nil {
		return fmt.Errorf("resolving global memory path: %w", err)
	}
	globalRanked, err := loadScoredEntries(globalPath, objKWs, now)
	if err != nil {
		return fmt.Errorf("loading global memory: %w", err)
	}

	budget := 0
	var selected []string
	for _, line := range globalRanked {
		lineBytes := len(line) + 1
		if budget+lineBytes > seedMemoryBudgetBytes {
			break
		}
		selected = append(selected, line)
		budget += lineBytes
	}

	var filteredContent []byte
	if len(selected) > 0 {
		filteredContent = []byte(strings.Join(selected, "\n") + "\n")
	}

	dst, err := workerMemoryPath(effectiveCWD)
	if err != nil {
		return fmt.Errorf("resolving worker memory path: %w", err)
	}

	if err := os.MkdirAll(filepath.Dir(dst), 0700); err != nil {
		return fmt.Errorf("creating worker memory dir: %w", err)
	}
	if err := os.WriteFile(dst, filteredContent, 0600); err != nil {
		return fmt.Errorf("writing worker MEMORY.md: %w", err)
	}
	if err := os.Chmod(dst, 0400); err != nil {
		return fmt.Errorf("chmod worker MEMORY.md: %w", err)
	}

	newPath, err := workerMemoryNewPath(effectiveCWD)
	if err != nil {
		return fmt.Errorf("resolving worker memory new path: %w", err)
	}
	if err := os.WriteFile(newPath, []byte(""), 0600); err != nil {
		return fmt.Errorf("creating worker MEMORY_NEW.md: %w", err)
	}
	return nil
}

// PromotePersonaEntries reads the persona canonical MEMORY.md, selects non-superseded
// entries for which matcher returns true, promotes them to the global MEMORY.md, and
// removes them from the persona canonical. Returns the count of promoted entries.
// If matcher is nil, all non-superseded entries are candidates.
func PromotePersonaEntries(personaName string, matcher func(*MemoryEntry) bool) (int, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return 0, fmt.Errorf("getting home dir: %w", err)
	}
	canonical := filepath.Join(home, "nanika", "personas", personaName, "MEMORY.md")
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
		if entryType != "project" && entryType != "reference" && entryType != "feedback" {
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
		if len(entryContent) > maxBridgedBytes {
			truncated := entryContent[:maxBridgedBytes]
			if idx := strings.LastIndexByte(truncated, '\n'); idx > 0 {
				truncated = truncated[:idx]
			}
			entryContent = truncated + "…"
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
