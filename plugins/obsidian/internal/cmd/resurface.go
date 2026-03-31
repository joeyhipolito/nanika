package cmd

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/config"
	"github.com/joeyhipolito/nanika-obsidian/internal/index"
	"github.com/joeyhipolito/nanika-obsidian/internal/output"
)

const defaultResurfaceOlderThan = "7d"
const defaultResurfaceLimit = 5

// ResurfaceOptions controls the resurface command behavior.
type ResurfaceOptions struct {
	Limit      int    // max results to return (default 5)
	OlderThan  string // duration string like "7d", "14d" (default "7d")
	Random     bool   // surface random old notes instead of query-based
	JSONOutput bool
}

// ResurfaceResult is a single resurfaced note.
type ResurfaceResult struct {
	Path    string  `json:"path"`
	Title   string  `json:"title"`
	Snippet string  `json:"snippet"`
	Score   float64 `json:"score"`
	ModTime int64   `json:"mod_time"`
	AgeDays int     `json:"age_days"`
}

// ResurfaceOutput is the JSON envelope for the resurface command.
type ResurfaceOutput struct {
	Query     string            `json:"query"`
	Mode      string            `json:"mode"`
	OlderThan string            `json:"older_than"`
	Results   []ResurfaceResult `json:"results"`
}

// ResurfaceCmd surfaces old notes that match the query or are randomly selected.
// In query mode, it runs a hybrid search and filters to notes older than the threshold.
// In random mode (opts.Random), it returns randomly selected old notes.
func ResurfaceCmd(vaultPath, query string, opts ResurfaceOptions) error {
	if opts.Limit <= 0 {
		opts.Limit = defaultResurfaceLimit
	}
	if opts.OlderThan == "" {
		opts.OlderThan = defaultResurfaceOlderThan
	}

	olderDuration, err := parseSinceDuration(opts.OlderThan)
	if err != nil {
		return fmt.Errorf("invalid --older value %q: %w", opts.OlderThan, err)
	}

	cutoff := time.Now().Add(-olderDuration).Unix()

	dbPath := index.IndexDBPath(vaultPath)
	store, err := index.Open(dbPath)
	if err != nil {
		return fmt.Errorf("failed to open index: %w\n\nRun 'obsidian index' to build the search index", err)
	}
	defer store.Close()

	count, _ := store.NoteCount()
	if count == 0 {
		if opts.JSONOutput {
			return output.JSON(ResurfaceOutput{
				Query:     query,
				Mode:      resurfaceMode(opts.Random),
				OlderThan: opts.OlderThan,
				Results:   []ResurfaceResult{},
			})
		}
		fmt.Println("No notes indexed. Run 'obsidian index' first.")
		return nil
	}

	var results []ResurfaceResult

	if opts.Random {
		rows, err := store.RandomOldNotes(cutoff, opts.Limit)
		if err != nil {
			return fmt.Errorf("failed to get random notes: %w", err)
		}
		results = noteRowsToResurfaceResults(rows, time.Now())
	} else {
		if query == "" {
			return fmt.Errorf("resurface requires a query or --random\n\nUsage: obsidian resurface <query> [flags]")
		}
		results, err = resurfaceByQuery(store, query, cutoff, opts.Limit, opts.JSONOutput)
		if err != nil {
			return err
		}
	}

	mode := resurfaceMode(opts.Random)

	if opts.JSONOutput {
		return output.JSON(ResurfaceOutput{
			Query:     query,
			Mode:      mode,
			OlderThan: opts.OlderThan,
			Results:   results,
		})
	}

	if len(results) == 0 {
		if opts.Random {
			fmt.Printf("No notes older than %s found.\n", opts.OlderThan)
		} else {
			fmt.Printf("No notes matching %q older than %s found.\n", query, opts.OlderThan)
		}
		return nil
	}

	if opts.Random {
		fmt.Printf("Random notes older than %s (%d found)\n\n", opts.OlderThan, len(results))
	} else {
		fmt.Printf("Resurface: %q — notes older than %s (%d found)\n\n", query, opts.OlderThan, len(results))
	}

	for i, r := range results {
		fmt.Printf("  %d. %s", i+1, r.Path)
		if r.Title != "" {
			fmt.Printf(" — %s", r.Title)
		}
		fmt.Printf("  (%d days old)\n", r.AgeDays)
		if r.Snippet != "" {
			fmt.Printf("     %s\n", r.Snippet)
		}
	}

	return nil
}

// resurfaceMode returns the mode string for output.
func resurfaceMode(random bool) string {
	if random {
		return "random"
	}
	return "query"
}

// resurfaceByQuery searches for notes matching query and filters to those older than cutoff.
func resurfaceByQuery(store *index.Store, query string, cutoff int64, limit int, jsonOutput bool) ([]ResurfaceResult, error) {
	// Fetch more candidates than needed so we have enough after age filtering.
	candidateLimit := limit * 4
	if candidateLimit < 20 {
		candidateLimit = 20
	}

	apiKey := config.ResolveAPIKey()
	embedClient := index.NewEmbeddingClient(apiKey)

	var searchResults []index.SearchResult
	var err error

	if embedClient.IsAvailable() {
		queryEmb, embErr := embedClient.Embed(context.Background(), query)
		if embErr != nil {
			if !jsonOutput {
				fmt.Printf("Warning: embedding failed, falling back to keyword search: %v\n", embErr)
			}
			searchResults, err = store.SearchKeyword(query, candidateLimit)
		} else {
			searchResults, err = store.SearchHybrid(query, queryEmb, candidateLimit)
		}
	} else {
		if !jsonOutput {
			fmt.Println("Warning: no Gemini API key — using keyword search only")
		}
		searchResults, err = store.SearchKeyword(query, candidateLimit)
	}
	if err != nil {
		return nil, fmt.Errorf("search failed: %w", err)
	}

	if len(searchResults) == 0 {
		return nil, nil
	}

	// Batch fetch mod_times for all candidates.
	paths := make([]string, len(searchResults))
	for i, r := range searchResults {
		paths[i] = r.Path
	}
	modTimes, err := store.GetModTimes(paths)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch note ages: %w", err)
	}

	now := time.Now()
	var results []ResurfaceResult
	for _, r := range searchResults {
		mt, ok := modTimes[r.Path]
		if !ok || mt == 0 || mt > cutoff {
			continue
		}
		ageDays := int(now.Unix()-mt) / 86400
		results = append(results, ResurfaceResult{
			Path:    r.Path,
			Title:   r.Title,
			Snippet: r.Snippet,
			Score:   r.Score,
			ModTime: mt,
			AgeDays: ageDays,
		})
		if len(results) >= limit {
			break
		}
	}

	return results, nil
}

// noteRowsToResurfaceResults converts NoteRows (from random query) to ResurfaceResults.
func noteRowsToResurfaceResults(rows []index.NoteRow, now time.Time) []ResurfaceResult {
	results := make([]ResurfaceResult, 0, len(rows))
	for _, n := range rows {
		ageDays := int(now.Unix()-n.ModTime) / 86400
		results = append(results, ResurfaceResult{
			Path:    n.Path,
			Title:   n.Title,
			Snippet: excerptBody(n.Body, 200),
			Score:   0,
			ModTime: n.ModTime,
			AgeDays: ageDays,
		})
	}
	return results
}

// excerptBody returns a short excerpt from body text for display as a snippet.
func excerptBody(body string, maxLen int) string {
	body = strings.TrimSpace(body)
	if len(body) <= maxLen {
		return body
	}
	cut := body[:maxLen]
	if i := strings.LastIndex(cut, " "); i > maxLen/2 {
		cut = cut[:i]
	}
	return cut + "…"
}
