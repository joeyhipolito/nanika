package cmd

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/index"
	"github.com/joeyhipolito/nanika-obsidian/internal/output"
	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// MaintainOutput represents the JSON output format for the maintain command.
type MaintainOutput struct {
	Stats          VaultStats  `json:"stats"`
	StaleNotes     []StaleNote `json:"stale_notes"`
	BrokenLinks    []BrokenLink `json:"broken_links"`
	EmptyNotes     []string    `json:"empty_notes"`
	LargeNotes     []LargeNote `json:"large_notes"`
	NoFrontmatter  []string    `json:"no_frontmatter"`
	InboxPending   int         `json:"inbox_pending"`
	InboxOldestDays int        `json:"inbox_oldest_days"`
	HealthScore    int         `json:"health_score"`
	Fixed          int         `json:"fixed"`
}

// VaultStats holds overall vault statistics.
type VaultStats struct {
	TotalNotes     int `json:"total_notes"`
	IndexedNotes   int `json:"indexed_notes"`
	WithEmbeddings int `json:"with_embeddings"`
	AvgSizeBytes   int `json:"avg_size_bytes"`
}

// StaleNote represents a note not modified recently.
type StaleNote struct {
	Path     string `json:"path"`
	LastMod  string `json:"last_modified"`
	DaysAgo  int    `json:"days_ago"`
}

// BrokenLink represents a wikilink pointing to a nonexistent note.
type BrokenLink struct {
	Source string `json:"source"`
	Target string `json:"target"`
}

// LargeNote represents a note exceeding the size threshold.
type LargeNote struct {
	Path      string `json:"path"`
	SizeBytes int64  `json:"size_bytes"`
}

// MaintainCmd performs vault health checks and reports issues.
func MaintainCmd(vaultPath string, staleDays int, fix, jsonOutput bool) error {
	notes, err := vault.ListNotes(vaultPath, "")
	if err != nil {
		return fmt.Errorf("failed to list notes: %w", err)
	}

	result := MaintainOutput{}
	result.Stats.TotalNotes = len(notes)

	// Get index stats if available
	dbPath := index.IndexDBPath(vaultPath)
	if _, err := os.Stat(dbPath); err == nil {
		store, err := index.Open(dbPath)
		if err == nil {
			defer store.Close()
			if count, err := store.NoteCount(); err == nil {
				result.Stats.IndexedNotes = count
			}
			if count, err := store.EmbeddingCount(); err == nil {
				result.Stats.WithEmbeddings = count
			}
		}
	}

	// Build lookup set of all note names (for broken link detection)
	noteNames := make(map[string]bool)
	for _, n := range notes {
		name := strings.TrimSuffix(filepath.Base(n.Path), ".md")
		noteNames[strings.ToLower(name)] = true
		// Also add full path without extension for path-based links
		pathNoExt := strings.TrimSuffix(n.Path, ".md")
		noteNames[strings.ToLower(pathNoExt)] = true
	}

	now := time.Now()
	var totalSize int64

	for _, info := range notes {
		totalSize += info.Size

		// Read note content for checks
		fullPath := filepath.Join(vaultPath, info.Path)
		data, err := os.ReadFile(fullPath)
		if err != nil {
			continue
		}
		content := string(data)

		// Check: stale notes
		modTime := time.Unix(info.ModTime, 0)
		daysOld := int(now.Sub(modTime).Hours() / 24)
		if daysOld >= staleDays {
			result.StaleNotes = append(result.StaleNotes, StaleNote{
				Path:    info.Path,
				LastMod: modTime.Format("2006-01-02"),
				DaysAgo: daysOld,
			})
		}

		// Check: broken wikilinks
		parsed := vault.ParseNote(content)
		for _, link := range parsed.Wikilinks {
			// Strip heading fragments
			target := link
			if idx := strings.Index(target, "#"); idx >= 0 {
				target = target[:idx]
			}
			target = strings.TrimSpace(target)
			if target == "" {
				continue
			}
			if !noteNames[strings.ToLower(target)] {
				result.BrokenLinks = append(result.BrokenLinks, BrokenLink{
					Source: info.Path,
					Target: link,
				})
			}
		}

		// Check: empty notes (only frontmatter, no body content)
		body := strings.TrimSpace(parsed.Body)
		if body == "" {
			result.EmptyNotes = append(result.EmptyNotes, info.Path)
		}

		// Check: large notes (>10KB)
		if info.Size > 10240 {
			result.LargeNotes = append(result.LargeNotes, LargeNote{
				Path:      info.Path,
				SizeBytes: info.Size,
			})
		}

		// Check: missing frontmatter
		if !strings.HasPrefix(content, "---\n") && !strings.HasPrefix(content, "---\r\n") {
			result.NoFrontmatter = append(result.NoFrontmatter, info.Path)
		}
	}

	if len(notes) > 0 {
		result.Stats.AvgSizeBytes = int(totalSize / int64(len(notes)))
	}

	// Scan inbox for pending triage
	inboxDir := filepath.Join(vaultPath, "Inbox")
	if _, err := os.Stat(inboxDir); err == nil {
		if inboxNotes, err := vault.ListNotes(vaultPath, "Inbox"); err == nil {
			for _, info := range inboxNotes {
				fullPath := filepath.Join(vaultPath, info.Path)
				data, err := os.ReadFile(fullPath)
				if err != nil {
					continue
				}
				parsed := vault.ParseNote(string(data))
				if frontmatterString(parsed.Frontmatter, "status") == "processed" {
					continue
				}
				result.InboxPending++
				ageDays, _ := computeAge(frontmatterString(parsed.Frontmatter, "created"), info.ModTime, now)
				if ageDays > result.InboxOldestDays {
					result.InboxOldestDays = ageDays
				}
			}
		}
	}

	// Calculate health score
	result.HealthScore = calculateHealthScore(result)

	// Apply fixes if requested
	if fix {
		result.Fixed = applyFixes(vaultPath, result)
	}

	if jsonOutput {
		return output.JSON(result)
	}

	printMaintainReport(result, fix)
	return nil
}

// calculateHealthScore computes a 0-100 health score.
func calculateHealthScore(r MaintainOutput) int {
	score := 100

	// Stale notes: -1 each, capped at -20
	staleDeduct := len(r.StaleNotes)
	if staleDeduct > 20 {
		staleDeduct = 20
	}
	score -= staleDeduct

	// Broken links: -2 each, capped at -20
	brokenDeduct := len(r.BrokenLinks) * 2
	if brokenDeduct > 20 {
		brokenDeduct = 20
	}
	score -= brokenDeduct

	// Empty notes: -5 each
	score -= len(r.EmptyNotes) * 5

	// Missing frontmatter: -3 each
	score -= len(r.NoFrontmatter) * 3

	// Index coverage
	if r.Stats.TotalNotes > 0 && r.Stats.IndexedNotes > 0 {
		coverage := float64(r.Stats.IndexedNotes) / float64(r.Stats.TotalNotes) * 100
		score -= int(100 - coverage)
	}

	// Inbox backlog: -1 per pending note, capped at -10
	inboxDeduct := r.InboxPending
	if inboxDeduct > 10 {
		inboxDeduct = 10
	}
	score -= inboxDeduct

	if score < 0 {
		score = 0
	}
	return score
}

// applyFixes adds frontmatter to notes missing it.
func applyFixes(vaultPath string, r MaintainOutput) int {
	fixed := 0
	for _, notePath := range r.NoFrontmatter {
		fullPath := filepath.Join(vaultPath, notePath)
		data, err := os.ReadFile(fullPath)
		if err != nil {
			continue
		}

		// Prepend empty frontmatter
		content := "---\n---\n" + string(data)
		if err := os.WriteFile(fullPath, []byte(content), 0644); err != nil {
			continue
		}
		fixed++
	}
	return fixed
}

func printMaintainReport(result MaintainOutput, fixed bool) {
	fmt.Println("Vault Health Report")
	fmt.Println(strings.Repeat("=", 40))

	// Stats
	fmt.Println("\nStats:")
	fmt.Printf("  Total notes: %d\n", result.Stats.TotalNotes)
	if result.Stats.IndexedNotes > 0 {
		pct := 0
		if result.Stats.TotalNotes > 0 {
			pct = result.Stats.IndexedNotes * 100 / result.Stats.TotalNotes
		}
		fmt.Printf("  Indexed: %d (%d%%)\n", result.Stats.IndexedNotes, pct)
	}
	if result.Stats.WithEmbeddings > 0 {
		pct := 0
		if result.Stats.TotalNotes > 0 {
			pct = result.Stats.WithEmbeddings * 100 / result.Stats.TotalNotes
		}
		fmt.Printf("  With embeddings: %d (%d%%)\n", result.Stats.WithEmbeddings, pct)
	}
	if result.Stats.AvgSizeBytes > 0 {
		fmt.Printf("  Average note size: %.1f KB\n", float64(result.Stats.AvgSizeBytes)/1024)
	}

	// Stale notes
	if len(result.StaleNotes) > 0 {
		fmt.Printf("\nStale Notes (not modified in 30+ days): %d\n", len(result.StaleNotes))
		for _, s := range result.StaleNotes {
			fmt.Printf("  - %s (last: %s, %d days ago)\n", s.Path, s.LastMod, s.DaysAgo)
		}
	}

	// Broken links
	if len(result.BrokenLinks) > 0 {
		fmt.Printf("\nBroken Wikilinks: %d\n", len(result.BrokenLinks))
		for _, bl := range result.BrokenLinks {
			fmt.Printf("  - %s links to [[%s]] (not found)\n", bl.Source, bl.Target)
		}
	}

	// Empty notes
	if len(result.EmptyNotes) > 0 {
		fmt.Printf("\nEmpty Notes: %d\n", len(result.EmptyNotes))
		for _, p := range result.EmptyNotes {
			fmt.Printf("  - %s\n", p)
		}
	}

	// Large notes
	if len(result.LargeNotes) > 0 {
		fmt.Printf("\nLarge Notes (>10KB): %d\n", len(result.LargeNotes))
		for _, ln := range result.LargeNotes {
			fmt.Printf("  - %s (%.1f KB)\n", ln.Path, float64(ln.SizeBytes)/1024)
		}
	}

	// Missing frontmatter
	if len(result.NoFrontmatter) > 0 {
		fmt.Printf("\nMissing Frontmatter: %d\n", len(result.NoFrontmatter))
		for _, p := range result.NoFrontmatter {
			fmt.Printf("  - %s\n", p)
		}
	}

	// Inbox triage status
	fmt.Println("\nInbox:")
	if result.InboxPending == 0 {
		fmt.Println("  Clear (no notes pending triage)")
	} else {
		fmt.Printf("  %d notes pending triage, oldest: %dd  → run: obsidian triage --auto\n",
			result.InboxPending, result.InboxOldestDays)
	}

	if fixed && result.Fixed > 0 {
		fmt.Printf("\nFixed: %d notes (frontmatter added)\n", result.Fixed)
	}

	fmt.Printf("\nHealth Score: %d/100\n", result.HealthScore)
}
