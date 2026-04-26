package cmd

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/joeyhipolito/nanika-obsidian/internal/index"
	"github.com/joeyhipolito/nanika-obsidian/internal/output"
)

// EnrichOutput represents the JSON output format for the enrich command.
type EnrichOutput struct {
	LinkSuggestions []LinkSuggestion `json:"link_suggestions"`
	TagSuggestions  []TagSuggestion  `json:"tag_suggestions"`
	OrphanNotes     []string         `json:"orphan_notes"`
	Summary         EnrichSummary    `json:"summary"`
}

// LinkSuggestion represents a suggested wikilink between two notes.
type LinkSuggestion struct {
	From       string  `json:"from"`
	To         string  `json:"to"`
	Similarity float64 `json:"similarity"`
}

// TagSuggestion represents a suggested tag for a note.
type TagSuggestion struct {
	Note string   `json:"note"`
	Tags []string `json:"tags"`
}

// EnrichSummary holds counts for the enrichment report.
type EnrichSummary struct {
	LinksFound  int `json:"links_found"`
	TagsFound   int `json:"tags_found"`
	OrphansFound int `json:"orphans_found"`
	Applied     int `json:"applied"`
}

// EnrichCmd analyzes the vault index and suggests connections between notes.
func EnrichCmd(vaultPath string, apply, jsonOutput bool) error {
	dbPath := index.IndexDBPath(vaultPath)
	if _, err := os.Stat(dbPath); err != nil {
		return fmt.Errorf("index not found — run 'obsidian index' first")
	}

	store, err := index.Open(dbPath)
	if err != nil {
		return fmt.Errorf("failed to open index: %w", err)
	}
	defer store.Close()

	notes, err := store.GetAllNoteRows()
	if err != nil {
		return fmt.Errorf("failed to load notes: %w", err)
	}

	if len(notes) == 0 {
		if jsonOutput {
			return output.JSON(EnrichOutput{})
		}
		fmt.Println("No indexed notes found. Run 'obsidian index' first.")
		return nil
	}

	result := EnrichOutput{}

	// Pass 1: Link suggestions via cosine similarity
	result.LinkSuggestions = findLinkSuggestions(notes)
	result.Summary.LinksFound = len(result.LinkSuggestions)

	// Pass 2: Tag suggestions via consensus filtering
	result.TagSuggestions = findTagSuggestions(notes)
	result.Summary.TagsFound = len(result.TagSuggestions)

	// Pass 3: Orphan detection
	result.OrphanNotes = findOrphans(notes)
	result.Summary.OrphansFound = len(result.OrphanNotes)

	// Apply link suggestions if requested
	if apply && len(result.LinkSuggestions) > 0 {
		applied := applyLinkSuggestions(vaultPath, result.LinkSuggestions)
		result.Summary.Applied = applied
	}

	if jsonOutput {
		return output.JSON(result)
	}

	printEnrichReport(result, apply)
	return nil
}

// findLinkSuggestions finds semantically similar notes that aren't already linked.
func findLinkSuggestions(notes []index.NoteRow) []LinkSuggestion {
	const threshold = 0.7
	const maxPerNote = 5

	// Build existing link sets for each note
	existingLinks := make(map[string]map[string]bool)
	for _, n := range notes {
		links := make(map[string]bool)
		if n.Wikilinks != "" {
			for _, link := range strings.Split(n.Wikilinks, ", ") {
				links[strings.ToLower(strings.TrimSpace(link))] = true
			}
		}
		existingLinks[n.Path] = links
	}

	// Build title-to-path lookup for link resolution
	titleToPath := make(map[string]string)
	for _, n := range notes {
		name := strings.TrimSuffix(filepath.Base(n.Path), ".md")
		titleToPath[strings.ToLower(name)] = n.Path
		if n.Title != "" {
			titleToPath[strings.ToLower(n.Title)] = n.Path
		}
	}

	// All-pairs cosine similarity (i < j to avoid duplicates)
	var suggestions []LinkSuggestion
	counts := make(map[string]int) // per-note suggestion count

	for i := 0; i < len(notes); i++ {
		if notes[i].Embedding == nil {
			continue
		}
		for j := i + 1; j < len(notes); j++ {
			if notes[j].Embedding == nil {
				continue
			}
			if counts[notes[i].Path] >= maxPerNote && counts[notes[j].Path] >= maxPerNote {
				continue
			}

			sim := float64(index.CosineSimilarity(notes[i].Embedding, notes[j].Embedding))
			if sim < threshold {
				continue
			}

			// Check if already linked (either direction)
			nameI := strings.TrimSuffix(filepath.Base(notes[i].Path), ".md")
			nameJ := strings.TrimSuffix(filepath.Base(notes[j].Path), ".md")
			if existingLinks[notes[i].Path][strings.ToLower(nameJ)] ||
				existingLinks[notes[j].Path][strings.ToLower(nameI)] {
				continue
			}

			if counts[notes[i].Path] < maxPerNote || counts[notes[j].Path] < maxPerNote {
				suggestions = append(suggestions, LinkSuggestion{
					From:       notes[i].Path,
					To:         notes[j].Path,
					Similarity: sim,
				})
				counts[notes[i].Path]++
				counts[notes[j].Path]++
			}
		}
	}

	// Sort by similarity descending
	for i := 1; i < len(suggestions); i++ {
		for j := i; j > 0 && suggestions[j].Similarity > suggestions[j-1].Similarity; j-- {
			suggestions[j], suggestions[j-1] = suggestions[j-1], suggestions[j]
		}
	}

	return suggestions
}

// findTagSuggestions suggests tags for notes based on consensus from similar notes.
func findTagSuggestions(notes []index.NoteRow) []TagSuggestion {
	const threshold = 0.7
	const consensusMin = 2 // tag must appear in 2+ similar notes

	var suggestions []TagSuggestion

	for i, note := range notes {
		if note.Embedding == nil {
			continue
		}

		// Get this note's existing tags
		existingTags := make(map[string]bool)
		if note.Tags != "" {
			for _, t := range strings.Split(note.Tags, ", ") {
				existingTags[strings.ToLower(strings.TrimSpace(t))] = true
			}
		}

		// Count tags from similar notes
		tagCounts := make(map[string]int)
		for j, other := range notes {
			if i == j || other.Embedding == nil || other.Tags == "" {
				continue
			}
			sim := float64(index.CosineSimilarity(note.Embedding, other.Embedding))
			if sim < threshold {
				continue
			}
			for _, t := range strings.Split(other.Tags, ", ") {
				t = strings.TrimSpace(t)
				if t != "" && !existingTags[strings.ToLower(t)] {
					tagCounts[strings.ToLower(t)]++
				}
			}
		}

		// Collect tags meeting consensus threshold
		var newTags []string
		for tag, count := range tagCounts {
			if count >= consensusMin {
				newTags = append(newTags, tag)
			}
		}

		if len(newTags) > 0 {
			suggestions = append(suggestions, TagSuggestion{
				Note: note.Path,
				Tags: newTags,
			})
		}
	}

	return suggestions
}

// findOrphans finds notes with no incoming wikilinks.
func findOrphans(notes []index.NoteRow) []string {
	// Build set of all notes that are linked TO
	linked := make(map[string]bool)
	for _, n := range notes {
		if n.Wikilinks == "" {
			continue
		}
		for _, link := range strings.Split(n.Wikilinks, ", ") {
			link = strings.TrimSpace(link)
			// Strip heading fragments
			if idx := strings.Index(link, "#"); idx >= 0 {
				link = link[:idx]
			}
			if link != "" {
				linked[strings.ToLower(link)] = true
			}
		}
	}

	// Find notes that nobody links to
	var orphans []string
	for _, n := range notes {
		name := strings.TrimSuffix(filepath.Base(n.Path), ".md")
		if !linked[strings.ToLower(name)] && !linked[strings.ToLower(n.Title)] {
			orphans = append(orphans, n.Path)
		}
	}

	return orphans
}

// applyLinkSuggestions appends suggested wikilinks to notes.
func applyLinkSuggestions(vaultPath string, suggestions []LinkSuggestion) int {
	// Group suggestions by source note
	byNote := make(map[string][]string)
	for _, s := range suggestions {
		toName := strings.TrimSuffix(filepath.Base(s.To), ".md")
		byNote[s.From] = append(byNote[s.From], toName)
		fromName := strings.TrimSuffix(filepath.Base(s.From), ".md")
		byNote[s.To] = append(byNote[s.To], fromName)
	}

	applied := 0
	for notePath, links := range byNote {
		fullPath := filepath.Join(vaultPath, notePath)
		data, err := os.ReadFile(fullPath)
		if err != nil {
			continue
		}

		content := string(data)

		// Build the links section
		var linkLines []string
		for _, link := range links {
			wikilink := fmt.Sprintf("- [[%s]]", link)
			if !strings.Contains(content, "[["+link+"]]") {
				linkLines = append(linkLines, wikilink)
			}
		}

		if len(linkLines) == 0 {
			continue
		}

		// Append to Related Notes section or add one
		newLinks := strings.Join(linkLines, "\n")
		if idx := strings.Index(content, "## Related Notes"); idx >= 0 {
			sectionStart := idx + len("## Related Notes")
			rest := content[sectionStart:]
			if nextIdx := strings.Index(rest, "\n##"); nextIdx >= 0 {
				// Insert before the next heading; step back past any blank line
				insertAt := sectionStart + nextIdx
				if insertAt > 0 && content[insertAt-1] == '\n' {
					insertAt--
				}
				content = content[:insertAt] + "\n" + newLinks + content[insertAt:]
			} else {
				content += "\n" + newLinks + "\n"
			}
		} else {
			content += "\n## Related Notes\n" + newLinks + "\n"
		}

		if err := os.WriteFile(fullPath, []byte(content), 0644); err != nil {
			continue
		}
		applied++
	}

	return applied
}

func printEnrichReport(result EnrichOutput, applied bool) {
	fmt.Println("Enrichment Report")
	fmt.Println(strings.Repeat("=", 40))

	if len(result.LinkSuggestions) > 0 {
		fmt.Println("\nSuggested Links:")
		for _, s := range result.LinkSuggestions {
			fromName := strings.TrimSuffix(filepath.Base(s.From), ".md")
			toName := strings.TrimSuffix(filepath.Base(s.To), ".md")
			fmt.Printf("  \"%s\" → \"%s\" (similarity: %.2f)\n", fromName, toName, s.Similarity)
		}
	}

	if len(result.TagSuggestions) > 0 {
		fmt.Println("\nSuggested Tags:")
		for _, s := range result.TagSuggestions {
			name := strings.TrimSuffix(filepath.Base(s.Note), ".md")
			fmt.Printf("  \"%s\" → add tags: [%s]\n", name, strings.Join(s.Tags, ", "))
		}
	}

	if len(result.OrphanNotes) > 0 {
		fmt.Println("\nOrphan Notes (no incoming links):")
		for _, p := range result.OrphanNotes {
			fmt.Printf("  - %s\n", p)
		}
	}

	fmt.Printf("\nSummary: %d link suggestions, %d tag suggestions, %d orphan notes",
		result.Summary.LinksFound, result.Summary.TagsFound, result.Summary.OrphansFound)
	if applied && result.Summary.Applied > 0 {
		fmt.Printf(", %d notes updated", result.Summary.Applied)
	}
	fmt.Println()
}
