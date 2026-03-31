package gather

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"
)

const testRSSFeed = `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Test Feed</title>
    <item>
      <title>Go 1.26 Released</title>
      <link>https://example.com/go-126</link>
      <description>Major release with new features</description>
      <pubDate>Mon, 17 Feb 2026 10:00:00 +0000</pubDate>
      <guid>https://example.com/go-126</guid>
    </item>
    <item>
      <title>Rust vs Go in 2026</title>
      <link>https://example.com/rust-vs-go</link>
      <description>Comparing two popular languages</description>
      <pubDate>Sun, 16 Feb 2026 08:00:00 +0000</pubDate>
    </item>
  </channel>
</rss>`

func TestGoogleNewsGatherer_Gather_HappyPath(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/xml")
		w.Write([]byte(testRSSFeed))
	}))
	defer ts.Close()

	g := NewGoogleNewsGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), []string{"Go"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(items) == 0 {
		t.Fatal("expected items, got none")
	}

	// All items should have "googlenews" tag
	for _, item := range items {
		hasTag := false
		for _, tag := range item.Tags {
			if tag == "googlenews" {
				hasTag = true
				break
			}
		}
		if !hasTag {
			t.Errorf("item %q missing googlenews tag", item.Title)
		}
	}
}

func TestGoogleNewsGatherer_Gather_EmptyTerms(t *testing.T) {
	g := NewGoogleNewsGatherer()
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if items != nil {
		t.Errorf("expected nil items for empty terms, got %d", len(items))
	}
}

func TestGoogleNewsGatherer_Gather_HTTPError(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusServiceUnavailable)
	}))
	defer ts.Close()

	g := NewGoogleNewsGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	_, err := g.Gather(context.Background(), []string{"test"})
	if err == nil {
		t.Fatal("expected error for 503 response")
	}
}

func TestGoogleNewsGatherer_Gather_InvalidRSS(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(`not xml at all`))
	}))
	defer ts.Close()

	g := NewGoogleNewsGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	_, err := g.Gather(context.Background(), []string{"test"})
	if err == nil {
		t.Fatal("expected error for invalid RSS")
	}
}

func TestGoogleNewsGatherer_Gather_FiltersByTerms(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testRSSFeed))
	}))
	defer ts.Close()

	g := NewGoogleNewsGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	// Search for "Rust" — should only return the Rust article
	items, err := g.Gather(context.Background(), []string{"Rust"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	for _, item := range items {
		if item.Title == "Go 1.26 Released" {
			t.Error("expected Go article to be filtered out when searching for Rust")
		}
	}
}
