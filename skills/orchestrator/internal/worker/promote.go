package worker

import (
	"context"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
)

// SyncLearningsToPersonas syncs high-quality learnings (quality_score > 0.7)
// from the learning database to persona MEMORY.md files. Learnings are converted
// to MemoryEntry format, deduplicated by content, and merged into the appropriate
// persona files based on worker_name. Once promoted, learnings are marked as
// promoted_at to prevent duplicate promotion.
func SyncLearningsToPersonas(ctx context.Context) (int, error) {
	// Open learning database
	db, err := learning.OpenDB("")
	if err != nil {
		return 0, fmt.Errorf("open learning DB: %w", err)
	}
	defer db.Close()

	// Find promotable learnings
	learnings, err := db.FindPromotable(ctx)
	if err != nil {
		return 0, fmt.Errorf("find promotable learnings: %w", err)
	}

	if len(learnings) == 0 {
		return 0, nil
	}

	// Group learnings by worker_name (persona)
	byPersona := make(map[string][]learning.Learning)
	for _, l := range learnings {
		persona := l.WorkerName
		if persona == "" {
			persona = "general" // fallback if no worker_name
		}
		byPersona[persona] = append(byPersona[persona], l)
	}

	promoted := 0

	// For each persona, merge learnings into MEMORY.md
	for persona, personaLearnings := range byPersona {
		n, err := mergeLearnersToPersonaMemory(ctx, db, persona, personaLearnings)
		if err != nil {
			// Log error but continue with other personas
			fmt.Fprintf(os.Stderr, "error syncing learnings to persona %q: %v\n", persona, err)
			continue
		}
		promoted += n
	}

	return promoted, nil
}

// mergeLearnersToPersonaMemory merges a list of learnings into a persona's MEMORY.md.
// Learnings are converted to MemoryEntry format, deduplicated by content hash,
// and appended to the persona's canonical MEMORY.md. Duplicate entries (same
// normalized content) are skipped. Returns the count of successfully promoted learnings.
func mergeLearnersToPersonaMemory(ctx context.Context, db *learning.DB, personaName string, learnings []learning.Learning) (int, error) {
	// Load existing entries from persona's MEMORY.md
	memPath, err := canonicalMemoryPath(personaName)
	if err != nil {
		return 0, fmt.Errorf("get canonical path for %q: %w", personaName, err)
	}

	// Load existing entries
	var existing []*MemoryEntry
	if _, err := os.Stat(memPath); err == nil {
		content, err := os.ReadFile(memPath)
		if err != nil {
			return 0, fmt.Errorf("read MEMORY.md for %q: %w", personaName, err)
		}
		for _, line := range strings.Split(string(content), "\n") {
			if strings.HasPrefix(strings.TrimSpace(line), "-") {
				// Index line format: "- [title](file.md) — description"
				continue
			}
			if e := ParseMemoryEntry(line); e != nil {
				existing = append(existing, e)
			}
		}
	}

	// Build hash map of existing entries for dedup
	existingHashes := make(map[string]bool)
	for _, e := range existing {
		hash := e.contentHash()
		if hash != "" {
			existingHashes[hash] = true
		}
	}

	// Convert learnings to MemoryEntry and filter duplicates
	var toAdd []*MemoryEntry
	var idsToMark []string

	for _, l := range learnings {
		entry := &MemoryEntry{
			Content: l.Content,
			Filed:   l.CreatedAt,
			By:      l.WorkerName,
			Type:    convertLearningType(l.Type),
		}

		hash := entry.contentHash()
		if hash != "" && existingHashes[hash] {
			// Skip duplicate
			continue
		}

		toAdd = append(toAdd, entry)
		idsToMark = append(idsToMark, l.ID)
		if hash != "" {
			existingHashes[hash] = true
		}
	}

	if len(toAdd) == 0 {
		return 0, nil
	}

	// Append new entries to MEMORY.md
	f, err := os.OpenFile(memPath, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0644)
	if err != nil {
		return 0, fmt.Errorf("open MEMORY.md for %q: %w", personaName, err)
	}
	defer f.Close()

	for _, entry := range toAdd {
		line := entry.String()
		if _, err := f.WriteString(line + "\n"); err != nil {
			return 0, fmt.Errorf("write to MEMORY.md for %q: %w", personaName, err)
		}
	}

	// Mark promoted learnings in DB
	for _, id := range idsToMark {
		if err := db.MarkPromoted(ctx, id); err != nil {
			return 0, fmt.Errorf("mark promoted for %s: %w", id, err)
		}
	}

	return len(toAdd), nil
}

// convertLearningType maps learning.LearningType to MemoryEntry.Type.
// The mapping aligns learning types with memory entry types where possible.
func convertLearningType(lt learning.LearningType) string {
	switch lt {
	case learning.TypePattern:
		return "pattern"
	case learning.TypeError:
		return "feedback" // error learnings map to feedback
	case learning.TypeSource:
		return "reference"
	case learning.TypeDecision:
		return "feedback"
	case learning.TypeInsight:
		return "user" // insights about the user's domain
	default:
		return "feedback"
	}
}
