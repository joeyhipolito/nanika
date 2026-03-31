package gather

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/browser"
)

// GoogleBrowserGatherer scrapes the Google personalized Discover feed via CDP.
// Requires Chrome running on localhost:9222 with an active Google session.
type GoogleBrowserGatherer struct{}

// NewGoogleBrowserGatherer creates a new Google Discover browser gatherer.
func NewGoogleBrowserGatherer() *GoogleBrowserGatherer {
	return &GoogleBrowserGatherer{}
}

func (g *GoogleBrowserGatherer) Name() string { return "google-browser" }

// Gather navigates to google.com, waits for body + 3-second SPA idle, gets
// innerHTML, and extracts external article links via the shared HTML extractor.
// Returns nil items (not an error) if Chrome is unavailable.
func (g *GoogleBrowserGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	client, err := browser.New(ctx, browser.DefaultCDPURL)
	if err != nil {
		fmt.Fprintf(os.Stderr, "    Warning: google-browser: Chrome unavailable (%v), skipping\n", err)
		return nil, nil
	}
	defer client.Close()

	// Navigate to google.com — the Discover feed appears below the search bar
	// when the user is signed in to their Google account.
	// Extract via JS: Discover feed links have long text (title + source + time)
	// and external URLs. Filter out short nav links (About, Gmail, etc.).
	var rawJSON string
	js := `JSON.stringify(Array.from(document.querySelectorAll('a[href]')).filter(function(a) {
		var text = a.textContent.trim();
		var href = a.href;
		// Discover items have substantial text (title + source + timestamp)
		if (text.length < 30) return false;
		// Skip Google internal links (nav, footer, apps)
		if (href.includes('accounts.google') || href.includes('support.google') ||
			href.includes('policies.google') || href.includes('google.com/preferences') ||
			href.includes('google.com/intl') || href.includes('google.com/search')) return false;
		// Skip "Skip to" accessibility links and sponsored content
		if (text.startsWith('Skip to')) return false;
		if (text.includes('Sponsored') || href.includes('google.com/aclk')) return false;
		return true;
	}).map(function(a) {
		var text = a.textContent.trim();
		// Try to split title from source+time — source is usually at the end after the last sentence
		var parts = text.split(/\s{2,}/);
		var title = parts[0] || text;
		var source = parts.length > 1 ? parts[parts.length - 1] : 'Google Discover';
		return {title: title.slice(0, 200), url: a.href, source: source};
	}))`
	err = client.Eval(ctx, "https://www.google.com", js, &rawJSON, browser.PageOptions{
		WaitSelector: "body",
		IdleTimeout:  5 * time.Second,
	})
	if err != nil {
		return nil, fmt.Errorf("google-browser: %w", err)
	}

	type jsItem struct {
		Title  string `json:"title"`
		URL    string `json:"url"`
		Source string `json:"source"`
	}
	var jsItems []jsItem
	if err := json.Unmarshal([]byte(rawJSON), &jsItems); err != nil {
		return nil, fmt.Errorf("google-browser: parsing JS result: %w", err)
	}

	type extractedItem = browser.ExtractedItem
	extracted := make([]extractedItem, 0, len(jsItems))
	for _, j := range jsItems {
		extracted = append(extracted, extractedItem{Title: j.Title, URL: j.URL, Source: j.Source})
	}

	now := time.Now().UTC()
	seen := make(map[string]bool)
	var items []IntelItem

	for _, e := range extracted {
		id := generateID(e.URL)
		if seen[id] {
			continue
		}
		seen[id] = true

		content := cleanText(e.Snippet)
		if len(content) > 500 {
			content = content[:500] + "..."
		}

		items = append(items, IntelItem{
			ID:        id,
			Title:     cleanText(e.Title),
			Content:   content,
			SourceURL: e.URL,
			Author:    e.Source,
			Timestamp: now,
			Tags:      []string{"google-browser", "discover"},
		})
	}

	// Browser sources skip search term filtering — the feed is already personalized
	return items, nil
}
