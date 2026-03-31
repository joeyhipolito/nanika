package gather

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/browser"
)

// XBrowserGatherer scrapes the X.com "For You" timeline via CDP.
// Requires Chrome running on localhost:9222 with an active X session.
type XBrowserGatherer struct{}

// NewXBrowserGatherer creates a new X For You browser gatherer.
func NewXBrowserGatherer() *XBrowserGatherer {
	return &XBrowserGatherer{}
}

func (g *XBrowserGatherer) Name() string { return "x-browser" }

// Gather navigates to x.com/home, waits for body + 3-second SPA idle,
// gets innerHTML, and extracts tweet links via the shared HTML extractor.
// Tweet permalink <a> elements contain a timestamp rather than tweet text;
// the extractor uses surrounding article content as the item title in that case.
// Returns nil items (not an error) if Chrome is unavailable.
func (g *XBrowserGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	client, err := browser.New(ctx, browser.DefaultCDPURL)
	if err != nil {
		fmt.Fprintf(os.Stderr, "    Warning: x-browser: Chrome unavailable (%v), skipping\n", err)
		return nil, nil
	}
	defer client.Close()

	// Extract tweets via JS — use article elements with data-testid selectors
	// which are X's stable semantic markers for tweet content
	var rawJSON string
	js := `JSON.stringify(Array.from(document.querySelectorAll('article')).map(function(a) {
		var textEl = a.querySelector('[data-testid="tweetText"]');
		var text = textEl ? textEl.textContent.trim() : '';
		var nameEl = a.querySelector('[data-testid="User-Name"]');
		var author = nameEl ? nameEl.textContent.trim().split('\n')[0] : '';
		var linkEl = a.querySelector('a[href*="/status/"]');
		var link = linkEl ? linkEl.href : '';
		// Skip ads/promoted tweets
		var isAd = a.querySelector('[data-testid="placementTracking"]') !== null;
		if (!text || text.length < 10 || isAd || !link) return null;
		return {text: text.slice(0, 500), author: author.slice(0, 80), link: link};
	}).filter(Boolean))`

	err = client.Eval(ctx, "https://x.com/home", js, &rawJSON, browser.PageOptions{
		WaitSelector: "article",
		IdleTimeout:  5 * time.Second,
	})
	if err != nil {
		return nil, fmt.Errorf("x-browser: %w", err)
	}

	type jsItem struct {
		Text   string `json:"text"`
		Author string `json:"author"`
		Link   string `json:"link"`
	}
	var jsItems []jsItem
	if err := json.Unmarshal([]byte(rawJSON), &jsItems); err != nil {
		return nil, fmt.Errorf("x-browser: parsing JS result: %w", err)
	}

	now := time.Now().UTC()
	seen := make(map[string]bool)
	var items []IntelItem

	for _, t := range jsItems {
		id := generateID(t.Link)
		if seen[id] {
			continue
		}
		seen[id] = true

		// Use first line or first 100 chars as title
		title := t.Text
		if len(title) > 100 {
			title = title[:100] + "..."
		}

		items = append(items, IntelItem{
			ID:        id,
			Title:     cleanText(title),
			Content:   cleanText(t.Text),
			SourceURL: t.Link,
			Author:    cleanText(t.Author),
			Timestamp: now,
			Tags:      []string{"x-browser", "for-you"},
		})
	}

	// Browser sources skip search term filtering — the feed is already personalized
	return items, nil
}
