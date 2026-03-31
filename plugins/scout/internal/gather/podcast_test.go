package gather

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"
)

// testPodcastFeed is a minimal RSS 2.0 podcast feed with iTunes extensions.
const testPodcastFeed = `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
  <channel>
    <title>Go Time</title>
    <item>
      <title>Standard title</title>
      <itunes:title>Go Concurrency Deep Dive</itunes:title>
      <link>https://changelog.com/gotime/1</link>
      <description>Standard description</description>
      <itunes:summary>An in-depth look at goroutines and channels in Go.</itunes:summary>
      <author>standard@example.com</author>
      <itunes:author>Mat Ryer</itunes:author>
      <pubDate>Mon, 17 Feb 2026 10:00:00 +0000</pubDate>
      <guid>https://changelog.com/gotime/1</guid>
    </item>
    <item>
      <title>Building CLI Tools with Go</title>
      <link>https://changelog.com/gotime/2</link>
      <description>How to build production-grade CLI tools using cobra and viper.</description>
      <itunes:author>Johnny Boursiquot</itunes:author>
      <pubDate>Sun, 16 Feb 2026 08:00:00 +0000</pubDate>
      <guid>https://changelog.com/gotime/2</guid>
    </item>
  </channel>
</rss>`

// testPodcastFeedNoItunes is a plain RSS 2.0 feed without iTunes extensions.
const testPodcastFeedNoItunes = `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Go Podcast</title>
    <item>
      <title>Go Modules Explained</title>
      <link>https://gopodcast.com/ep/1</link>
      <description>Everything you need to know about Go modules.</description>
      <author>gopodcast@example.com</author>
      <pubDate>Fri, 14 Feb 2026 12:00:00 +0000</pubDate>
      <guid>gopodcast-ep-1</guid>
    </item>
  </channel>
</rss>`

// testPodcastFeedEnclosure has an episode with no <link> but an <enclosure> URL.
const testPodcastFeedEnclosure = `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Audio Only Podcast</title>
    <item>
      <title>Episode With Audio</title>
      <enclosure url="https://cdn.example.com/ep1.mp3" type="audio/mpeg"/>
      <description>An episode without a web link.</description>
      <pubDate>Thu, 13 Feb 2026 09:00:00 +0000</pubDate>
      <guid>audio-only-ep-1</guid>
    </item>
  </channel>
</rss>`

// testPodcastFeedLongDesc has an episode with a very long description (>500 chars).
const testPodcastFeedLongDesc = `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Verbose Podcast</title>
    <item>
      <title>Long Description Episode</title>
      <link>https://example.com/ep/1</link>
      <description>This is a very long description that exceeds the 500 character truncation limit. Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. More text here to ensure we exceed the limit comfortably.</description>
      <guid>long-ep-1</guid>
    </item>
  </channel>
</rss>`

func TestPodcastGatherer_Gather_ParsesEpisodes(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/rss+xml")
		w.Write([]byte(testPodcastFeed))
	}))
	defer ts.Close()

	g := NewPodcastGatherer([]string{ts.URL + "/feed"})
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 2 {
		t.Fatalf("expected 2 items, got %d", len(items))
	}
}

func TestPodcastGatherer_Gather_PrefersItunesFields(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testPodcastFeed))
	}))
	defer ts.Close()

	g := NewPodcastGatherer([]string{ts.URL + "/feed"})
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) == 0 {
		t.Fatal("no items returned")
	}

	first := items[0]
	if first.Title != "Go Concurrency Deep Dive" {
		t.Errorf("expected iTunes title, got %q", first.Title)
	}
	if first.Content != "An in-depth look at goroutines and channels in Go." {
		t.Errorf("expected iTunes summary as content, got %q", first.Content)
	}
	if first.Author != "Mat Ryer" {
		t.Errorf("expected iTunes author, got %q", first.Author)
	}
}

func TestPodcastGatherer_Gather_FallsBackToStandardFields(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testPodcastFeedNoItunes))
	}))
	defer ts.Close()

	g := NewPodcastGatherer([]string{ts.URL + "/feed"})
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) == 0 {
		t.Fatal("no items returned")
	}

	item := items[0]
	if item.Title != "Go Modules Explained" {
		t.Errorf("expected standard title, got %q", item.Title)
	}
	if item.Content != "Everything you need to know about Go modules." {
		t.Errorf("expected standard description as content, got %q", item.Content)
	}
}

func TestPodcastGatherer_Gather_UsesEnclosureWhenNoLink(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testPodcastFeedEnclosure))
	}))
	defer ts.Close()

	g := NewPodcastGatherer([]string{ts.URL + "/feed"})
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) == 0 {
		t.Fatal("no items returned")
	}
	if items[0].SourceURL != "https://cdn.example.com/ep1.mp3" {
		t.Errorf("expected enclosure URL as source, got %q", items[0].SourceURL)
	}
}

func TestPodcastGatherer_Gather_TagsIncludePodcastName(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testPodcastFeed))
	}))
	defer ts.Close()

	g := NewPodcastGatherer([]string{ts.URL + "/feed"})
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	for _, item := range items {
		hasPodcast := false
		hasName := false
		for _, tag := range item.Tags {
			if tag == "podcast" {
				hasPodcast = true
			}
			if tag == "Go Time" {
				hasName = true
			}
		}
		if !hasPodcast {
			t.Errorf("item %q missing 'podcast' tag, tags: %v", item.Title, item.Tags)
		}
		if !hasName {
			t.Errorf("item %q missing podcast name tag, tags: %v", item.Title, item.Tags)
		}
	}
}

func TestPodcastGatherer_Gather_FallsBackToPodcastNameAsAuthor(t *testing.T) {
	const noAuthorFeed = `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Anonymous Podcast</title>
    <item>
      <title>Episode 1</title>
      <link>https://example.com/ep1</link>
      <guid>ep-1</guid>
    </item>
  </channel>
</rss>`

	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(noAuthorFeed))
	}))
	defer ts.Close()

	g := NewPodcastGatherer([]string{ts.URL + "/feed"})
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) == 0 {
		t.Fatal("no items returned")
	}
	if items[0].Author != "Anonymous Podcast" {
		t.Errorf("expected podcast name as fallback author, got %q", items[0].Author)
	}
}

func TestPodcastGatherer_Gather_TruncatesLongContent(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testPodcastFeedLongDesc))
	}))
	defer ts.Close()

	g := NewPodcastGatherer([]string{ts.URL + "/feed"})
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) == 0 {
		t.Fatal("no items returned")
	}
	if len(items[0].Content) > 503 {
		t.Errorf("expected content truncated, got %d chars", len(items[0].Content))
	}
}

func TestPodcastGatherer_Gather_HTTPError_ContinuesGracefully(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNotFound)
	}))
	defer ts.Close()

	g := NewPodcastGatherer([]string{ts.URL + "/missing"})
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("HTTP error should not propagate as error, got: %v", err)
	}
	if len(items) != 0 {
		t.Errorf("expected 0 items for failed feed, got %d", len(items))
	}
}

func TestPodcastGatherer_Gather_MalformedXML_ContinuesGracefully(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(`this is not xml`))
	}))
	defer ts.Close()

	g := NewPodcastGatherer([]string{ts.URL + "/bad"})
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("malformed XML should not propagate as error, got: %v", err)
	}
	if len(items) != 0 {
		t.Errorf("expected 0 items for malformed feed, got %d", len(items))
	}
}

func TestPodcastGatherer_Gather_DeduplicatesAcrossFeeds(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testPodcastFeed))
	}))
	defer ts.Close()

	// Two feeds returning the same content — items should be deduped by ID
	g := NewPodcastGatherer([]string{ts.URL + "/feed1", ts.URL + "/feed2"})
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	seen := make(map[string]bool)
	for _, item := range items {
		if seen[item.ID] {
			t.Errorf("duplicate item ID %s after dedup", item.ID)
		}
		seen[item.ID] = true
	}
}

func TestPodcastGatherer_Gather_FiltersBySearchTerms(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testPodcastFeed))
	}))
	defer ts.Close()

	g := NewPodcastGatherer([]string{ts.URL + "/feed"})
	items, err := g.Gather(context.Background(), []string{"concurrency"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	for _, item := range items {
		if item.Title == "Building CLI Tools with Go" {
			t.Error("CLI episode should be filtered out when searching for 'concurrency'")
		}
	}
}

func TestPodcastGatherer_Gather_EmptyFeedList_ReturnsEmpty(t *testing.T) {
	g := NewPodcastGatherer(nil)
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 0 {
		t.Errorf("expected 0 items with no feeds, got %d", len(items))
	}
}

func TestPodcastGatherer_Gather_EngagementIsZero(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(testPodcastFeed))
	}))
	defer ts.Close()

	g := NewPodcastGatherer([]string{ts.URL + "/feed"})
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	for _, item := range items {
		if item.Engagement != 0 {
			t.Errorf("expected Engagement=0 for podcast item, got %d", item.Engagement)
		}
	}
}

func TestPodcastGatherer_Name(t *testing.T) {
	g := NewPodcastGatherer(nil)
	if g.Name() != "podcast" {
		t.Errorf("expected name 'podcast', got %q", g.Name())
	}
}

func TestParsePodcastFeed_StableIDs(t *testing.T) {
	data := []byte(testPodcastFeed)
	items1, err := parsePodcastFeed(data)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	items2, err := parsePodcastFeed(data)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	for i := range items1 {
		if items1[i].ID != items2[i].ID {
			t.Errorf("IDs not stable: %q vs %q", items1[i].ID, items2[i].ID)
		}
	}
}

func TestPodcastGatherer_Gather_HTTP304_ReturnsEmpty(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNotModified)
	}))
	defer ts.Close()

	g := NewPodcastGatherer([]string{ts.URL + "/feed"})
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("304 should not be an error, got: %v", err)
	}
	if len(items) != 0 {
		t.Errorf("expected 0 items for 304 response, got %d", len(items))
	}
}
