package gather

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
)

// LobstersGatherer fetches stories from Lobste.rs tag RSS feeds.
type LobstersGatherer struct {
	Tags   []string
	Client *http.Client
}

// NewLobstersGatherer creates a new Lobsters gatherer with tag filters.
func NewLobstersGatherer(tags []string) *LobstersGatherer {
	return &LobstersGatherer{Tags: tags, Client: newHTTPClient()}
}

// Name returns the canonical source identifier.
func (g *LobstersGatherer) Name() string { return "lobsters" }

// Gather fetches Lobste.rs stories from configured tag RSS feeds.
func (g *LobstersGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	tags := g.Tags
	if len(tags) == 0 {
		tags = searchTerms
	}
	if len(tags) == 0 {
		return nil, nil
	}
	return collectFromList(tags, "lobsters", func(tag string) ([]IntelItem, error) {
		items, err := g.fetchTagFeed(ctx, tag)
		if err != nil {
			return nil, err
		}
		for i := range items {
			items[i].Tags = append(items[i].Tags, "lobsters")
		}
		return items, nil
	}, searchTerms)
}

// fetchTagFeed fetches and parses a Lobste.rs tag RSS feed.
func (g *LobstersGatherer) fetchTagFeed(ctx context.Context, tag string) ([]IntelItem, error) {
	feedURL := fmt.Sprintf("https://lobste.rs/t/%s.rss", url.PathEscape(strings.ToLower(tag)))

	req, err := http.NewRequestWithContext(ctx, "GET", feedURL, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}
	req.Header.Set("User-Agent", userAgent)

	resp, err := g.Client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch Lobsters feed: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d from Lobsters", resp.StatusCode)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response: %w", err)
	}

	// Lobste.rs uses standard RSS 2.0 — reuse parseFeed
	return parseFeed(body)
}
