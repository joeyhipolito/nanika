package cmd

import (
	"context"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-obsidian/internal/config"
	"github.com/joeyhipolito/nanika-obsidian/internal/index"
	"github.com/joeyhipolito/nanika-obsidian/internal/output"
	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// batchSize is the max number of texts to embed in a single API call.
const batchSize = 100

// IndexOutput represents the JSON output format for the index command.
type IndexOutput struct {
	NotesIndexed int    `json:"notes_indexed"`
	NotesSkipped int    `json:"notes_skipped"`
	NotesRemoved int    `json:"notes_removed"`
	TotalNotes   int    `json:"total_notes"`
	Errors       int    `json:"errors"`
	DBPath       string `json:"db_path"`
}

// IndexCmd builds or updates the SQLite search index for the vault.
// Crawls vault, parses frontmatter/headings/wikilinks, builds FTS5 index,
// and generates Gemini vector embeddings for semantic search.
// Uses mtime tracking for incremental indexing.
func IndexCmd(vaultPath string, jsonOutput bool) error {
	dbPath := index.IndexDBPath(vaultPath)

	store, err := index.Open(dbPath)
	if err != nil {
		return fmt.Errorf("failed to open index: %w", err)
	}
	defer store.Close()

	// Set up embedding client
	apiKey := config.ResolveAPIKey()
	embedClient := index.NewEmbeddingClient(apiKey)

	if !embedClient.IsAvailable() && !jsonOutput {
		fmt.Println("Warning: Gemini API key not configured — indexing without embeddings")
		fmt.Println("Run 'obsidian configure' to set up your API key for semantic search")
	}

	// List all notes in the vault
	notes, err := vault.ListNotes(vaultPath, "")
	if err != nil {
		return fmt.Errorf("failed to list vault notes: %w", err)
	}

	var stats IndexOutput
	stats.DBPath = dbPath

	// Collect notes that need indexing (new or modified)
	type noteWork struct {
		info vault.NoteInfo
		note *vault.Note
		row  *index.NoteRow
	}
	var toIndex []noteWork

	for _, info := range notes {
		storedMtime, err := store.GetModTime(info.Path)
		if err != nil {
			stats.Errors++
			continue
		}

		// Skip if not modified since last index
		if storedMtime >= info.ModTime {
			stats.NotesSkipped++
			continue
		}

		// Read and parse the note
		parsed, err := vault.ReadNote(vaultPath, info.Path)
		if err != nil {
			if !jsonOutput {
				fmt.Fprintf(os.Stderr, "  error reading %s: %v\n", info.Path, err)
			}
			stats.Errors++
			continue
		}

		// Extract metadata
		title := extractTitle(parsed, info.Name)
		tags := extractTags(parsed)
		headings := extractHeadingTexts(parsed)
		wikilinks := strings.Join(parsed.Wikilinks, ", ")

		row := &index.NoteRow{
			Path:      info.Path,
			Title:     title,
			Tags:      tags,
			Headings:  headings,
			Wikilinks: wikilinks,
			Body:      parsed.Body,
			ModTime:   info.ModTime,
		}

		toIndex = append(toIndex, noteWork{info: info, note: parsed, row: row})
	}

	// Generate embeddings in batches if API key is available
	if embedClient.IsAvailable() && len(toIndex) > 0 {
		if !jsonOutput {
			fmt.Printf("Generating embeddings for %d notes...\n", len(toIndex))
		}

		// Build text batch
		texts := make([]string, len(toIndex))
		for i, w := range toIndex {
			texts[i] = index.BuildSearchText(w.row.Title, w.row.Tags, w.row.Headings, w.row.Body)
		}

		// Process in batches
		ctx := context.Background()
		for start := 0; start < len(texts); start += batchSize {
			end := start + batchSize
			if end > len(texts) {
				end = len(texts)
			}

			embeddings, err := embedClient.EmbedBatch(ctx, texts[start:end])
			if err != nil {
				if !jsonOutput {
					fmt.Printf("  embedding batch error: %v\n", err)
				}
				stats.Errors += end - start
				continue
			}

			for i, emb := range embeddings {
				toIndex[start+i].row.Embedding = emb
			}
		}
	}

	// Write all notes to the index
	for _, w := range toIndex {
		if err := store.UpsertNote(w.row); err != nil {
			if !jsonOutput {
				fmt.Printf("  error indexing %s: %v\n", w.row.Path, err)
			}
			stats.Errors++
			continue
		}
		stats.NotesIndexed++
	}

	// Remove notes that no longer exist in the vault
	vaultPaths := make(map[string]bool, len(notes))
	for _, n := range notes {
		vaultPaths[n.Path] = true
	}

	indexedPaths, err := store.GetAllPaths()
	if err == nil {
		for path := range indexedPaths {
			if !vaultPaths[path] {
				if err := store.DeleteNote(path); err == nil {
					stats.NotesRemoved++
				}
			}
		}
	}

	total, _ := store.NoteCount()
	stats.TotalNotes = total

	if jsonOutput {
		return output.JSON(stats)
	}

	fmt.Printf("Index updated: %d indexed, %d skipped, %d removed (%d total, %d errors)\n",
		stats.NotesIndexed, stats.NotesSkipped, stats.NotesRemoved, stats.TotalNotes, stats.Errors)
	fmt.Printf("Database: %s\n", dbPath)
	return nil
}

// extractTitle gets the note title from frontmatter or filename.
func extractTitle(note *vault.Note, fallback string) string {
	if t, ok := note.Frontmatter["title"].(string); ok && t != "" {
		return t
	}
	// Use first H1 heading if available
	for _, h := range note.Headings {
		if h.Level == 1 {
			return h.Text
		}
	}
	return fallback
}

// extractTags gets tags from frontmatter as a comma-separated string.
func extractTags(note *vault.Note) string {
	switch v := note.Frontmatter["tags"].(type) {
	case []string:
		return strings.Join(v, ", ")
	case string:
		return v
	default:
		return ""
	}
}

// extractHeadingTexts gets all heading texts as a newline-separated string.
func extractHeadingTexts(note *vault.Note) string {
	if len(note.Headings) == 0 {
		return ""
	}
	texts := make([]string, len(note.Headings))
	for i, h := range note.Headings {
		texts[i] = h.Text
	}
	return strings.Join(texts, "\n")
}
