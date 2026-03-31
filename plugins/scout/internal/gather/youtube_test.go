package gather

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"
)

// testYouTubeAtomFeed is a minimal YouTube channel Atom feed.
const testYouTubeAtomFeed = `<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test Channel</title>
  <entry>
    <id>yt:video:abc123</id>
    <title>Go Concurrency Explained</title>
    <link rel="alternate" href="https://www.youtube.com/watch?v=abc123"/>
    <published>2026-02-17T10:00:00+00:00</published>
    <author><name>GoDevChannel</name></author>
    <summary>Deep dive into goroutines and channels</summary>
  </entry>
  <entry>
    <id>yt:video:def456</id>
    <title>Building CLI Tools with Go</title>
    <link rel="alternate" href="https://www.youtube.com/watch?v=def456"/>
    <published>2026-02-16T08:00:00+00:00</published>
    <author><name>GoDevChannel</name></author>
    <summary>How to build production CLI tools</summary>
  </entry>
</feed>`

func TestYouTubeGatherer_Gather_ChannelFeed(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/atom+xml")
		w.Write([]byte(testYouTubeAtomFeed))
	}))
	defer ts.Close()

	g := NewYouTubeGatherer([]string{"UCtest123"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}
	// Point news gatherer to same test server to avoid network calls
	g.news.Client = g.Client

	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(items) == 0 {
		t.Fatal("expected items from channel feed, got none")
	}

	// All items should have "youtube" tag
	for _, item := range items {
		hasTag := false
		for _, tag := range item.Tags {
			if tag == "youtube" {
				hasTag = true
				break
			}
		}
		if !hasTag {
			t.Errorf("item %q missing youtube tag, tags: %v", item.Title, item.Tags)
		}
	}
}

func TestYouTubeGatherer_Gather_NoChannels_NoTerms(t *testing.T) {
	g := NewYouTubeGatherer(nil)
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 0 {
		t.Errorf("expected 0 items with no channels and no terms, got %d", len(items))
	}
}

func TestYouTubeGatherer_Gather_HTTPError_ContinuesGracefully(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusForbidden)
	}))
	defer ts.Close()

	g := NewYouTubeGatherer([]string{"UCbadchannel"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}
	g.news.Client = g.Client

	// HTTP error on channel feed is a warning, not fatal
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 0 {
		t.Errorf("expected 0 items for failed channel, got %d", len(items))
	}
}

func TestYouTubeGatherer_Gather_DeduplicatesAcrossChannels(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testYouTubeAtomFeed))
	}))
	defer ts.Close()

	// Two channels returning same feed — IDs should dedup
	g := NewYouTubeGatherer([]string{"UC111", "UC222"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}
	g.news.Client = g.Client

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

func TestYouTubeGatherer_Gather_FiltersBySearchTerms(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testYouTubeAtomFeed))
	}))
	defer ts.Close()

	g := NewYouTubeGatherer([]string{"UCtest"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}
	g.news.Client = g.Client

	// Only "Concurrency" articles should survive filtering
	items, err := g.Gather(context.Background(), []string{"Concurrency"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	for _, item := range items {
		if item.Title == "Building CLI Tools with Go" {
			t.Error("expected CLI article filtered out when searching for Concurrency")
		}
	}
}

func TestYouTubeGatherer_Gather_EngagementIsZero(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testYouTubeAtomFeed))
	}))
	defer ts.Close()

	g := NewYouTubeGatherer([]string{"UCtest"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}
	g.news.Client = g.Client

	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	for _, item := range items {
		if item.Engagement != 0 {
			t.Errorf("expected engagement=0 for YouTube RSS item, got %d", item.Engagement)
		}
	}
}
