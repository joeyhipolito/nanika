package gather

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

func blueskyTestResponse(posts []blueskyPost) []byte {
	resp := blueskySearchResponse{Posts: posts}
	b, _ := json.Marshal(resp)
	return b
}

var testBlueskyPosts = []blueskyPost{
	{
		URI: "at://did:plc:abc123/app.bsky.feed.post/rkey1",
		Author: blueskyAuthor{
			Handle:      "gopher.bsky.social",
			DisplayName: "Go Gopher",
		},
		Record: blueskyRecord{
			Text:      "Go 1.26 just dropped with exciting new features for the ecosystem",
			CreatedAt: "2026-02-17T10:00:00Z",
		},
		LikeCount:   42,
		RepostCount: 10,
	},
	{
		URI: "at://did:plc:def456/app.bsky.feed.post/rkey2",
		Author: blueskyAuthor{
			Handle: "rustacean.bsky.social",
		},
		Record: blueskyRecord{
			Text:      "Rust async runtime improvements in the latest release",
			CreatedAt: "2026-02-16T08:00:00Z",
		},
		LikeCount:   15,
		RepostCount: 5,
	},
}

func TestBlueskyGatherer_Gather_HappyPath(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write(blueskyTestResponse(testBlueskyPosts))
	}))
	defer ts.Close()

	g := NewBlueskyGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), []string{"Go"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(items) == 0 {
		t.Fatal("expected items, got none")
	}

	// All items should have "bluesky" tag
	for _, item := range items {
		hasTag := false
		for _, tag := range item.Tags {
			if tag == "bluesky" {
				hasTag = true
				break
			}
		}
		if !hasTag {
			t.Errorf("item %q missing bluesky tag", item.Title)
		}
	}
}

func TestBlueskyGatherer_Gather_EmptyTerms(t *testing.T) {
	g := NewBlueskyGatherer()
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if items != nil {
		t.Errorf("expected nil items for empty search terms, got %d", len(items))
	}
}

func TestBlueskyGatherer_Gather_HTTPError_ReturnsError(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
	}))
	defer ts.Close()

	g := NewBlueskyGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	_, err := g.Gather(context.Background(), []string{"golang"})
	if err == nil {
		t.Fatal("expected error when all Bluesky queries fail")
	}
}

func TestBlueskyGatherer_Gather_RateLimit(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusTooManyRequests)
	}))
	defer ts.Close()

	g := NewBlueskyGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	_, err := g.Gather(context.Background(), []string{"test"})
	if err == nil {
		t.Fatal("expected error for 429 rate limit response")
	}
}

func TestBlueskyGatherer_Gather_DeduplicatesAcrossTerms(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// All search terms return the same posts
		w.Write(blueskyTestResponse(testBlueskyPosts))
	}))
	defer ts.Close()

	g := NewBlueskyGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	// Two search terms both return the same items — should dedup by ID
	items, err := g.Gather(context.Background(), []string{"Go", "golang"})
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

func TestBlueskyGatherer_Gather_EngagementFromLikesAndReposts(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write(blueskyTestResponse(testBlueskyPosts))
	}))
	defer ts.Close()

	g := NewBlueskyGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), []string{"Go"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	// First post: 42 likes + 10 reposts = 52 engagement
	for _, item := range items {
		if item.Author == "Go Gopher" && item.Engagement != 52 {
			t.Errorf("expected engagement=52 for first post, got %d", item.Engagement)
		}
	}
}

func TestATURIToWebURL(t *testing.T) {
	tests := []struct {
		atURI  string
		handle string
		want   string
	}{
		{
			atURI:  "at://did:plc:abc123/app.bsky.feed.post/rkey1",
			handle: "gopher.bsky.social",
			want:   "https://bsky.app/profile/gopher.bsky.social/post/rkey1",
		},
		{
			atURI:  "at://did:plc:abc123/app.bsky.feed.post/rkey1",
			handle: "", // no handle — falls back to DID
			want:   "https://bsky.app/profile/did:plc:abc123/post/rkey1",
		},
		{
			atURI:  "not-an-at-uri",
			handle: "someone",
			want:   "not-an-at-uri", // returned as-is
		},
	}

	for _, tc := range tests {
		got := atURIToWebURL(tc.atURI, tc.handle)
		if got != tc.want {
			t.Errorf("atURIToWebURL(%q, %q) = %q, want %q", tc.atURI, tc.handle, got, tc.want)
		}
	}
}

func TestTruncateBskyTitle(t *testing.T) {
	tests := []struct {
		text   string
		maxLen int
		want   string
	}{
		{"short text", 100, "short text"},
		{"word1 word2 word3", 10, "word1..."},
		{"exactly ten!", 12, "exactly ten!"},
		{"  whitespace  ", 100, "whitespace"},
	}

	for _, tc := range tests {
		got := truncateBskyTitle(tc.text, tc.maxLen)
		if got != tc.want {
			t.Errorf("truncateBskyTitle(%q, %d) = %q, want %q", tc.text, tc.maxLen, got, tc.want)
		}
	}
}

func TestBlueskyPostToIntel_FallsBackToIndexedAt(t *testing.T) {
	post := blueskyPost{
		URI:    "at://did:plc:xxx/app.bsky.feed.post/abc",
		Author: blueskyAuthor{Handle: "user.bsky.social"},
		Record: blueskyRecord{
			Text:      "test post",
			CreatedAt: "", // no createdAt
		},
		IndexedAt: "2026-02-17T12:00:00Z",
	}

	item := blueskyPostToIntel(post)
	if item.Timestamp.IsZero() {
		t.Error("expected timestamp from IndexedAt fallback, got zero")
	}
}

func TestBlueskyPostToIntel_UsesDisplayNameOverHandle(t *testing.T) {
	post := blueskyPost{
		URI: "at://did:plc:xxx/app.bsky.feed.post/abc",
		Author: blueskyAuthor{
			Handle:      "handle.bsky.social",
			DisplayName: "Display Name",
		},
		Record: blueskyRecord{Text: "post", CreatedAt: "2026-02-17T00:00:00Z"},
	}

	item := blueskyPostToIntel(post)
	if item.Author != "Display Name" {
		t.Errorf("expected DisplayName as author, got %q", item.Author)
	}
}

func TestBlueskyPostToIntel_FallsBackToHandle(t *testing.T) {
	post := blueskyPost{
		URI:    "at://did:plc:xxx/app.bsky.feed.post/abc",
		Author: blueskyAuthor{Handle: "handle.bsky.social", DisplayName: ""},
		Record: blueskyRecord{Text: "post", CreatedAt: "2026-02-17T00:00:00Z"},
	}

	item := blueskyPostToIntel(post)
	if item.Author != "handle.bsky.social" {
		t.Errorf("expected handle as fallback author, got %q", item.Author)
	}
}
