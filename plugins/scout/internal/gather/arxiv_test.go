package gather

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"
)

// testArxivAtomFeed is a minimal ArXiv API Atom response.
const testArxivAtomFeed = `<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>ArXiv Query Results</title>
  <entry>
    <id>http://arxiv.org/abs/2502.00001v1</id>
    <title>Attention Is All You Still Need</title>
    <link href="http://arxiv.org/abs/2502.00001v1" rel="alternate"/>
    <published>2026-02-17T00:00:00Z</published>
    <author><name>Alice Researcher</name></author>
    <summary>A new paper on attention mechanisms in 2026.</summary>
  </entry>
  <entry>
    <id>http://arxiv.org/abs/2502.00002v1</id>
    <title>Scaling Laws for Large Language Models</title>
    <link href="http://arxiv.org/abs/2502.00002v1" rel="alternate"/>
    <published>2026-02-16T00:00:00Z</published>
    <author><name>Bob Scientist</name></author>
    <summary>Revisiting scaling laws with new data.</summary>
  </entry>
</feed>`

func TestArxivGatherer_Gather_ByCategory(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/atom+xml")
		w.Write([]byte(testArxivAtomFeed))
	}))
	defer ts.Close()

	g := NewArxivGatherer([]string{"cs.AI"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(items) == 0 {
		t.Fatal("expected items from ArXiv category query, got none")
	}

	// All items should have "arxiv" tag
	for _, item := range items {
		hasTag := false
		for _, tag := range item.Tags {
			if tag == "arxiv" {
				hasTag = true
				break
			}
		}
		if !hasTag {
			t.Errorf("item %q missing arxiv tag, tags: %v", item.Title, item.Tags)
		}
	}
}

func TestArxivGatherer_Gather_FreetextSearch(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testArxivAtomFeed))
	}))
	defer ts.Close()

	// No categories — falls back to free-text search using search terms
	g := NewArxivGatherer(nil)
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), []string{"attention", "transformer"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(items) == 0 {
		t.Fatal("expected items from free-text ArXiv search, got none")
	}
}

func TestArxivGatherer_Gather_NoCategoriesNoTerms(t *testing.T) {
	g := NewArxivGatherer(nil)
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 0 {
		t.Errorf("expected 0 items with no categories and no terms, got %d", len(items))
	}
}

func TestArxivGatherer_Gather_HTTPError_ReturnsError(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusServiceUnavailable)
	}))
	defer ts.Close()

	g := NewArxivGatherer([]string{"cs.LG"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	_, err := g.Gather(context.Background(), nil)
	if err == nil {
		t.Fatal("expected error when all ArXiv queries fail")
	}
}

func TestArxivGatherer_Gather_RateLimit(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusTooManyRequests)
	}))
	defer ts.Close()

	g := NewArxivGatherer([]string{"stat.ML"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	_, err := g.Gather(context.Background(), nil)
	if err == nil {
		t.Fatal("expected error for 429 rate limit response")
	}
}

func TestArxivGatherer_Gather_DeduplicatesAcrossCategories(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testArxivAtomFeed))
	}))
	defer ts.Close()

	// Two categories returning identical feeds — should dedup by ID
	g := NewArxivGatherer([]string{"cs.AI", "cs.LG"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	seen := make(map[string]bool)
	for _, item := range items {
		if seen[item.ID] {
			t.Errorf("duplicate item ID %s found after dedup", item.ID)
		}
		seen[item.ID] = true
	}
}

func TestArxivGatherer_Gather_EngagementIsZero(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testArxivAtomFeed))
	}))
	defer ts.Close()

	g := NewArxivGatherer([]string{"cs.AI"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	for _, item := range items {
		if item.Engagement != 0 {
			t.Errorf("expected engagement=0 for ArXiv item, got %d", item.Engagement)
		}
	}
}

func TestBuildArxivQuery(t *testing.T) {
	tests := []struct {
		cat         string
		searchTerms []string
		want        string
	}{
		{"cs.AI", nil, "cat:cs.AI"},
		{"cs.AI", []string{"LLM"}, "cat:cs.AI+AND+(all:LLM)"},
		{"cs.AI", []string{"LLM", "transformer"}, "cat:cs.AI+AND+(all:LLM+OR+all:transformer)"},
		{"", []string{"LLM", "transformer"}, "all:LLM+OR+all:transformer"},
		{"", []string{"multi word term"}, "all:multi+word+term"},
	}

	for _, tc := range tests {
		got := buildArxivQuery(tc.cat, tc.searchTerms)
		if got != tc.want {
			t.Errorf("buildArxivQuery(%q, %v) = %q, want %q", tc.cat, tc.searchTerms, got, tc.want)
		}
	}
}
