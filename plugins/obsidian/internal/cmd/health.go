package cmd

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/output"
	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

const staleCaptureThresholdDays = 7

// HealthOutput represents the JSON output for the health command.
type HealthOutput struct {
	TotalNotes             int            `json:"total_notes"`
	InboxDepth             int            `json:"inbox_depth"`
	StaleCaptures          int            `json:"stale_captures"`
	OrphanNotes            int            `json:"orphan_notes"`
	ClassificationDist     map[string]int `json:"classification_distribution"`
	LinkDensity            float64        `json:"link_density"`
}

// HealthCmd reports vault diagnostics without modifying any content.
func HealthCmd(vaultPath string, jsonOutput bool) error {
	notes, err := vault.ListNotes(vaultPath, "")
	if err != nil {
		return fmt.Errorf("failed to list notes: %w", err)
	}

	result := HealthOutput{
		TotalNotes:         len(notes),
		ClassificationDist: noteClassificationDist(notes),
	}

	// Single pass: read every note once, building the inbound-link map and
	// caching parsed frontmatter for the Inbox metrics loop below.
	inboundLinks := make(map[string]int)
	parsedByPath := make(map[string]*vault.Note, len(notes))
	totalLinks := 0
	now := time.Now()

	for _, info := range notes {
		fullPath := filepath.Join(vaultPath, info.Path)
		data, err := os.ReadFile(fullPath)
		if err != nil {
			continue
		}
		parsed := vault.ParseNote(string(data))
		parsedByPath[info.Path] = parsed
		totalLinks += len(parsed.Wikilinks)
		for _, link := range parsed.Wikilinks {
			target := link
			if idx := strings.Index(target, "#"); idx >= 0 {
				target = target[:idx]
			}
			target = strings.TrimSpace(strings.ToLower(target))
			if target != "" {
				inboundLinks[target]++
			}
		}
	}

	result.OrphanNotes = countOrphans(notes, inboundLinks)
	result.LinkDensity = avgWikilinkDensity(totalLinks, len(notes))

	// Inbox metrics: depth (pending) and stale captures (>7d untriaged).
	// Filter from the already-fetched note list; reuse cached parsed notes.
	for _, info := range notes {
		if !strings.HasPrefix(info.Path, "Inbox/") {
			continue
		}
		parsed, ok := parsedByPath[info.Path]
		if !ok {
			continue
		}
		if frontmatterString(parsed.Frontmatter, "status") == "processed" {
			continue
		}
		result.InboxDepth++
		ageDays, _ := computeAge(frontmatterString(parsed.Frontmatter, "created"), info.ModTime, now)
		if ageDays > staleCaptureThresholdDays {
			result.StaleCaptures++
		}
	}

	if jsonOutput {
		return output.JSON(result)
	}

	printHealthReport(result)
	return nil
}

// countOrphans returns the number of notes with no inbound wikilinks.
// inboundLinks maps lowercased note names/paths to their inbound reference count.
func countOrphans(notes []vault.NoteInfo, inboundLinks map[string]int) int {
	count := 0
	for _, info := range notes {
		name := strings.ToLower(strings.TrimSuffix(filepath.Base(info.Path), ".md"))
		pathNoExt := strings.ToLower(strings.TrimSuffix(info.Path, ".md"))
		if inboundLinks[name] == 0 && inboundLinks[pathNoExt] == 0 {
			count++
		}
	}
	return count
}

// noteClassificationDist counts notes per top-level folder.
// Notes at vault root are counted under "Root".
func noteClassificationDist(notes []vault.NoteInfo) map[string]int {
	dist := make(map[string]int)
	for _, info := range notes {
		parts := strings.SplitN(info.Path, "/", 2)
		folder := parts[0]
		if len(parts) == 1 {
			folder = "Root"
		}
		dist[folder]++
	}
	return dist
}

// avgWikilinkDensity returns the average number of wikilinks per note.
func avgWikilinkDensity(totalLinks, noteCount int) float64 {
	if noteCount == 0 {
		return 0
	}
	return float64(totalLinks) / float64(noteCount)
}

func printHealthReport(r HealthOutput) {
	fmt.Println("Vault Health")
	fmt.Println(strings.Repeat("=", 40))
	fmt.Printf("\n  Total notes:      %d\n", r.TotalNotes)
	fmt.Printf("  Inbox depth:      %d pending", r.InboxDepth)
	if r.StaleCaptures > 0 {
		fmt.Printf(" (%d stale >%dd)", r.StaleCaptures, staleCaptureThresholdDays)
	}
	fmt.Println()
	fmt.Printf("  Orphan notes:     %d\n", r.OrphanNotes)
	fmt.Printf("  Link density:     %.2f wikilinks/note\n", r.LinkDensity)

	if len(r.ClassificationDist) > 0 {
		fmt.Println("\nClassification Distribution:")
		// Sort folders for deterministic output.
		folders := make([]string, 0, len(r.ClassificationDist))
		for f := range r.ClassificationDist {
			folders = append(folders, f)
		}
		sort.Strings(folders)
		for _, folder := range folders {
			fmt.Printf("  %-20s %d\n", folder+"/", r.ClassificationDist[folder])
		}
	}
}
