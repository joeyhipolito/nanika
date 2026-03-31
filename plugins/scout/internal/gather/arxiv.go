package gather

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"time"
)

// ArxivGatherer fetches papers from the ArXiv open API using category and
// free-text search. No API key required.
type ArxivGatherer struct {
	Categories []string
	Client     *http.Client
}

// NewArxivGatherer creates a new ArXiv gatherer for the given categories
// (e.g. "cs.AI", "cs.LG", "stat.ML").
func NewArxivGatherer(categories []string) *ArxivGatherer {
	return &ArxivGatherer{Categories: categories, Client: newHTTPClient()}
}

// Name returns the canonical source identifier.
func (g *ArxivGatherer) Name() string { return "arxiv" }

// Gather fetches ArXiv papers by configured categories combined with search
// terms, or by free-text search alone when no categories are configured.
// Applies 2s rate limiting between requests.
func (g *ArxivGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	seen := make(map[string]bool)
	var all []IntelItem
	var lastErr error

	for i, cat := range g.Categories {
		if i > 0 {
			select {
			case <-time.After(2 * time.Second):
			case <-ctx.Done():
				return nil, ctx.Err()
			}
		}
		items, err := g.fetchQuery(ctx, buildArxivQuery(cat, searchTerms))
		if err != nil {
			lastErr = err
			fmt.Fprintf(os.Stderr, "    Warning: arxiv/%s: %v\n", cat, err)
			continue
		}
		all = appendUnique(seen, all, items)
	}

	if len(g.Categories) == 0 && len(searchTerms) > 0 {
		items, err := g.fetchQuery(ctx, buildArxivQuery("", searchTerms))
		if err != nil {
			lastErr = err
			fmt.Fprintf(os.Stderr, "    Warning: arxiv/search: %v\n", err)
		} else {
			all = appendUnique(seen, all, items)
		}
	}

	if len(all) == 0 && lastErr != nil {
		return nil, fmt.Errorf("arxiv: all queries failed, last: %w", lastErr)
	}
	return all, nil
}

// fetchQuery performs a single ArXiv API query and returns parsed items.
func (g *ArxivGatherer) fetchQuery(ctx context.Context, searchQuery string) ([]IntelItem, error) {
	if searchQuery == "" {
		return nil, nil
	}

	// ArXiv requires + for spaces within search_query; do not use url.QueryEscape
	// which would produce %20 and break the boolean operators.
	apiURL := fmt.Sprintf(
		"https://export.arxiv.org/api/query?search_query=%s&start=0&max_results=25&sortBy=submittedDate&sortOrder=descending",
		searchQuery,
	)

	req, err := http.NewRequestWithContext(ctx, "GET", apiURL, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}
	req.Header.Set("User-Agent", userAgent)

	resp, err := g.Client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch ArXiv API: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusTooManyRequests {
		return nil, fmt.Errorf("rate limited (429)")
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d", resp.StatusCode)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response: %w", err)
	}

	items, err := parseFeed(body)
	if err != nil {
		return nil, fmt.Errorf("failed to parse ArXiv Atom feed: %w", err)
	}

	// Tag all items with "arxiv" and zero out engagement (not available)
	for i := range items {
		items[i].Tags = append(items[i].Tags, "arxiv")
		items[i].Engagement = 0
	}

	return items, nil
}

// buildArxivQuery constructs an ArXiv search_query string.
// Uses + for spaces (ArXiv boolean syntax) rather than URL percent-encoding.
//
// Examples:
//
//	buildArxivQuery("cs.AI", nil)                       → "cat:cs.AI"
//	buildArxivQuery("cs.AI", []string{"LLM"})           → "cat:cs.AI+AND+(all:LLM)"
//	buildArxivQuery("", []string{"LLM", "transformer"}) → "all:LLM+OR+all:transformer"
func buildArxivQuery(cat string, searchTerms []string) string {
	var termParts []string
	for _, t := range searchTerms {
		// Replace spaces within a term with + for ArXiv syntax
		termParts = append(termParts, "all:"+strings.ReplaceAll(t, " ", "+"))
	}
	termsQuery := strings.Join(termParts, "+OR+")

	switch {
	case cat != "" && len(termParts) > 0:
		return "cat:" + cat + "+AND+(" + termsQuery + ")"
	case cat != "":
		return "cat:" + cat
	default:
		return termsQuery
	}
}
