package gather

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"
)

const testLobstersRSS = `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Lobsters: go</title>
    <item>
      <title>A Fast Go HTTP Router</title>
      <link>https://example.com/go-router</link>
      <description>High-performance HTTP routing for Go applications</description>
      <author>poster@example.com</author>
      <pubDate>Mon, 17 Feb 2026 12:00:00 +0000</pubDate>
      <guid>https://lobste.rs/s/abc123</guid>
      <category>go</category>
      <category>programming</category>
    </item>
    <item>
      <title>Go Error Handling Patterns</title>
      <link>https://example.com/go-errors</link>
      <description>Best practices for error handling in Go</description>
      <pubDate>Sun, 16 Feb 2026 08:00:00 +0000</pubDate>
      <guid>https://lobste.rs/s/def456</guid>
      <category>go</category>
    </item>
  </channel>
</rss>`

func TestLobstersGatherer_Gather_HappyPath(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/xml")
		w.Write([]byte(testLobstersRSS))
	}))
	defer ts.Close()

	g := NewLobstersGatherer([]string{"go"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), []string{"Go"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(items) == 0 {
		t.Fatal("expected items, got none")
	}

	// All items should have "lobsters" tag
	for _, item := range items {
		hasTag := false
		for _, tag := range item.Tags {
			if tag == "lobsters" {
				hasTag = true
				break
			}
		}
		if !hasTag {
			t.Errorf("item %q missing lobsters tag", item.Title)
		}
	}
}

func TestLobstersGatherer_Gather_EmptyTagsAndTerms(t *testing.T) {
	g := NewLobstersGatherer(nil)
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if items != nil {
		t.Errorf("expected nil items for empty tags/terms, got %d", len(items))
	}
}

func TestLobstersGatherer_Gather_FallbackToSearchTerms(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testLobstersRSS))
	}))
	defer ts.Close()

	// No configured tags — should fall back to search terms
	g := NewLobstersGatherer(nil)
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), []string{"go"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(items) == 0 {
		t.Fatal("expected items when falling back to search terms")
	}
}

func TestLobstersGatherer_Gather_HTTPError(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNotFound)
	}))
	defer ts.Close()

	g := NewLobstersGatherer([]string{"nonexistent"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	// HTTP errors on tag feeds are warnings, not fatal — returns empty results
	items, err := g.Gather(context.Background(), []string{"test"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 0 {
		t.Errorf("expected 0 items for failed feed, got %d", len(items))
	}
}

func TestLobstersGatherer_Gather_InvalidRSS(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(`not valid xml`))
	}))
	defer ts.Close()

	g := NewLobstersGatherer([]string{"go"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	// Invalid RSS is a per-tag warning, not a fatal error
	items, err := g.Gather(context.Background(), []string{"test"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 0 {
		t.Errorf("expected 0 items for invalid RSS, got %d", len(items))
	}
}

func TestLobstersGatherer_Gather_Dedup(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testLobstersRSS))
	}))
	defer ts.Close()

	// Two tags that return the same items — should dedup
	g := NewLobstersGatherer([]string{"go", "programming"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), []string{"Go"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	// Both tags return same RSS — IDs should dedup
	seen := make(map[string]bool)
	for _, item := range items {
		if seen[item.ID] {
			t.Errorf("duplicate item ID %s found", item.ID)
		}
		seen[item.ID] = true
	}
}

func TestLobstersGatherer_Gather_PreservesCategories(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testLobstersRSS))
	}))
	defer ts.Close()

	g := NewLobstersGatherer([]string{"go"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), []string{"Go", "router"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	// The first item should have RSS categories ("go", "programming") plus "lobsters"
	for _, item := range items {
		if item.Title == "A Fast Go HTTP Router" {
			hasGo := false
			for _, tag := range item.Tags {
				if tag == "go" {
					hasGo = true
				}
			}
			if !hasGo {
				t.Error("expected RSS category 'go' to be preserved as tag")
			}
			return
		}
	}
}
