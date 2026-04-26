package cmd

import (
	"context"
	"fmt"

	"github.com/joeyhipolito/nanika-obsidian/internal/config"
	"github.com/joeyhipolito/nanika-obsidian/internal/index"
	"github.com/joeyhipolito/nanika-obsidian/internal/output"
)

// SearchOutput represents the JSON output format for the search command.
type SearchOutput struct {
	Query   string               `json:"query"`
	Mode    string               `json:"mode"`
	Results []index.SearchResult `json:"results"`
}

// SearchCmd searches notes using keyword (FTS5), semantic (vector), or hybrid search.
// mode: "keyword", "semantic", or "hybrid" (default).
func SearchCmd(vaultPath, query, mode string, jsonOutput bool) error {
	if mode == "" {
		mode = "hybrid"
	}

	dbPath := index.IndexDBPath(vaultPath)
	store, err := index.Open(dbPath)
	if err != nil {
		return fmt.Errorf("failed to open index: %w\n\nRun 'obsidian index' to build the search index", err)
	}
	defer store.Close()

	// Check index has notes
	count, _ := store.NoteCount()
	if count == 0 {
		if jsonOutput {
			return output.JSON(SearchOutput{Query: query, Mode: mode, Results: []index.SearchResult{}})
		}
		fmt.Println("No notes indexed. Run 'obsidian index' first.")
		return nil
	}

	const limit = 20
	var results []index.SearchResult

	switch mode {
	case "keyword":
		results, err = store.SearchKeyword(query, limit)
		if err != nil {
			return fmt.Errorf("keyword search failed: %w", err)
		}

	case "semantic":
		apiKey := config.ResolveAPIKey()
		embedClient := index.NewEmbeddingClient(apiKey)
		if !embedClient.IsAvailable() {
			return fmt.Errorf("semantic search requires a Gemini API key\n\nRun 'obsidian configure' to set up")
		}

		queryEmb, err := embedClient.Embed(context.Background(), query)
		if err != nil {
			return fmt.Errorf("failed to embed query: %w", err)
		}

		results, err = store.SearchSemantic(queryEmb, limit)
		if err != nil {
			return fmt.Errorf("semantic search failed: %w", err)
		}

	case "hybrid":
		apiKey := config.ResolveAPIKey()
		embedClient := index.NewEmbeddingClient(apiKey)

		if embedClient.IsAvailable() {
			queryEmb, err := embedClient.Embed(context.Background(), query)
			if err != nil {
				// Fall back to keyword-only if embedding fails
				if !jsonOutput {
					fmt.Printf("Warning: embedding failed, falling back to keyword search: %v\n", err)
				}
				results, err = store.SearchKeyword(query, limit)
				if err != nil {
					return fmt.Errorf("keyword search failed: %w", err)
				}
			} else {
				results, err = store.SearchHybrid(query, queryEmb, limit)
				if err != nil {
					return fmt.Errorf("hybrid search failed: %w", err)
				}
			}
		} else {
			// No API key — fall back to keyword search
			if !jsonOutput {
				fmt.Println("Warning: no Gemini API key — using keyword search only")
			}
			mode = "keyword"
			results, err = store.SearchKeyword(query, limit)
			if err != nil {
				return fmt.Errorf("keyword search failed: %w", err)
			}
		}

	default:
		return fmt.Errorf("unknown search mode: %s (use keyword, semantic, or hybrid)", mode)
	}

	if jsonOutput {
		return output.JSON(SearchOutput{
			Query:   query,
			Mode:    mode,
			Results: results,
		})
	}

	if len(results) == 0 {
		fmt.Printf("No results for %q (%s mode)\n", query, mode)
		return nil
	}

	fmt.Printf("Search: %q (%s mode, %d results)\n\n", query, mode, len(results))
	for i, r := range results {
		fmt.Printf("  %d. %s", i+1, r.Path)
		if r.Title != "" {
			fmt.Printf(" — %s", r.Title)
		}
		fmt.Printf("  (%.4f)\n", r.Score)
		if r.Snippet != "" {
			fmt.Printf("     %s\n", r.Snippet)
		}
	}

	return nil
}
