package gather

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
)

// GoogleNewsGatherer fetches articles from Google News RSS.
type GoogleNewsGatherer struct {
	Client *http.Client
}

// NewGoogleNewsGatherer creates a new Google News gatherer.
func NewGoogleNewsGatherer() *GoogleNewsGatherer {
	return &GoogleNewsGatherer{Client: newHTTPClient()}
}

// Name returns the canonical source identifier.
func (g *GoogleNewsGatherer) Name() string { return "googlenews" }

// Gather searches Google News RSS for articles matching the search terms.
func (g *GoogleNewsGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	if len(searchTerms) == 0 {
		return nil, nil
	}
	items, err := g.fetchGoogleNews(ctx, strings.Join(searchTerms, " OR "))
	if err != nil {
		return nil, fmt.Errorf("googlenews: %w", err)
	}
	for i := range items {
		items[i].Tags = append(items[i].Tags, "googlenews")
	}
	return filterByTerms(items, searchTerms), nil
}

// fetchGoogleNews fetches and parses a Google News RSS feed.
func (g *GoogleNewsGatherer) fetchGoogleNews(ctx context.Context, query string) ([]IntelItem, error) {
	feedURL := fmt.Sprintf(
		"https://news.google.com/rss/search?q=%s&hl=en-US&gl=US&ceid=US:en",
		url.QueryEscape(query),
	)

	req, err := http.NewRequestWithContext(ctx, "GET", feedURL, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}
	req.Header.Set("User-Agent", userAgent)

	resp, err := g.Client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch Google News: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d from Google News", resp.StatusCode)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response: %w", err)
	}

	// Google News uses standard RSS 2.0 — reuse parseFeed
	return parseFeed(body)
}
