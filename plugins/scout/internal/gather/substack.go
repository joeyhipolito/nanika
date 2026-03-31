package gather

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
)

// defaultSubstackPubs are well-known tech publications used as fallback
// when no specific publications are configured.
var defaultSubstackPubs = []string{
	"pragmaticengineer",
	"bytebytego",
	"lenny",
	"platformer",
	"semaphoreci",
	"ainews",
}

// SubstackGatherer fetches posts from Substack publication RSS feeds
// and optionally discovers posts via Google News site-search.
type SubstackGatherer struct {
	Publications []string
	Client       *http.Client
}

// NewSubstackGatherer creates a new Substack gatherer with the given publications.
func NewSubstackGatherer(publications []string) *SubstackGatherer {
	return &SubstackGatherer{Publications: publications, Client: newHTTPClient()}
}

// Name returns the canonical source identifier.
func (g *SubstackGatherer) Name() string { return "substack" }

// Gather fetches posts from configured Substack publications and optionally
// searches for broader Substack content via Google News.
func (g *SubstackGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	pubs := g.Publications
	if len(pubs) == 0 {
		pubs = defaultSubstackPubs
	}
	seen := make(map[string]bool)
	var all []IntelItem

	for _, pub := range pubs {
		items, err := g.fetchFeed(ctx, fmt.Sprintf("https://%s.substack.com/feed", pub), pub)
		if err != nil {
			fmt.Fprintf(os.Stderr, "    Warning: substack/%s: %v\n", pub, err)
			continue
		}
		all = appendUnique(seen, all, items)
	}

	if len(searchTerms) > 0 {
		if items, err := g.searchSubstack(ctx, searchTerms); err != nil {
			fmt.Fprintf(os.Stderr, "    Warning: substack search: %v\n", err)
		} else {
			all = appendUnique(seen, all, items)
		}
	}

	return filterByTerms(all, searchTerms), nil
}

// fetchFeed fetches and parses an RSS feed from a Substack publication.
func (g *SubstackGatherer) fetchFeed(ctx context.Context, feedURL, pubName string) ([]IntelItem, error) {
	req, err := http.NewRequestWithContext(ctx, "GET", feedURL, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}
	req.Header.Set("User-Agent", userAgent)

	resp, err := g.Client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch %s: %w", feedURL, err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d from %s", resp.StatusCode, feedURL)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response: %w", err)
	}

	items, err := parseFeed(body)
	if err != nil {
		return nil, fmt.Errorf("failed to parse feed: %w", err)
	}

	// Tag all items with substack and publication name
	for i := range items {
		items[i].Tags = append(items[i].Tags, "substack", pubName)
		// Truncate content if needed
		if len(items[i].Content) > 500 {
			items[i].Content = items[i].Content[:500] + "..."
		}
	}

	return items, nil
}

// searchSubstack searches for Substack content using Google News RSS.
// Searches each term individually for better recall.
func (g *SubstackGatherer) searchSubstack(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	seen := make(map[string]bool)
	var allItems []IntelItem
	var lastErr error

	for _, term := range searchTerms {
		query := url.QueryEscape("site:substack.com " + term)
		feedURL := fmt.Sprintf("https://news.google.com/rss/search?q=%s&hl=en-US&gl=US&ceid=US:en", query)

		items, err := g.fetchSearchFeed(ctx, feedURL)
		if err != nil {
			lastErr = err
			fmt.Fprintf(os.Stderr, "    Warning: substack/search/%s: %v\n", term, err)
			continue
		}
		for _, item := range items {
			if !seen[item.ID] {
				seen[item.ID] = true
				// Only keep items that are actually from substack.com
				if strings.Contains(item.SourceURL, "substack.com") {
					item.Tags = append(item.Tags, "substack", "search")
					if len(item.Content) > 500 {
						item.Content = item.Content[:500] + "..."
					}
					allItems = append(allItems, item)
				}
			}
		}
	}

	if len(allItems) == 0 && lastErr != nil {
		return nil, fmt.Errorf("substack search: all queries failed, last: %w", lastErr)
	}

	return allItems, nil
}

// fetchSearchFeed fetches and parses an RSS search feed.
func (g *SubstackGatherer) fetchSearchFeed(ctx context.Context, feedURL string) ([]IntelItem, error) {
	req, err := http.NewRequestWithContext(ctx, "GET", feedURL, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}
	req.Header.Set("User-Agent", "Mozilla/5.0 (compatible; scout-cli/0.4)")

	resp, err := g.Client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("substack search: failed to fetch: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("substack search: HTTP %d", resp.StatusCode)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("substack search: failed to read response: %w", err)
	}

	return parseFeed(body)
}
