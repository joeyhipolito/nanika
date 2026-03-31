package gather

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

func xTestResponse(tweets []xTweet, users []xUser) []byte {
	resp := xSearchResponse{
		Data:     tweets,
		Includes: xIncludes{Users: users},
		Meta:     xSearchMeta{ResultCount: len(tweets)},
	}
	b, _ := json.Marshal(resp)
	return b
}

var (
	testXUsers = []xUser{
		{ID: "100", Name: "Go Team", Username: "golang"},
		{ID: "200", Name: "", Username: "rustlang"},
	}

	testXTweets = []xTweet{
		{
			ID:        "111",
			Text:      "Go 1.26 brings exciting new features for the ecosystem",
			AuthorID:  "100",
			CreatedAt: "2026-02-17T10:00:00Z",
			PublicMetrics: xPublicMetrics{
				LikeCount:    42,
				RetweetCount: 10,
				QuoteCount:   3,
			},
		},
		{
			ID:        "222",
			Text:      "Rust async runtime improvements in the latest release",
			AuthorID:  "200",
			CreatedAt: "2026-02-16T08:00:00Z",
			PublicMetrics: xPublicMetrics{
				LikeCount:    15,
				RetweetCount: 5,
				QuoteCount:   0,
			},
		},
	}
)

func TestXGatherer_Gather_HappyPath(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// Verify auth header is present
		if r.Header.Get("Authorization") != "Bearer test-token" {
			w.WriteHeader(http.StatusUnauthorized)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		w.Write(xTestResponse(testXTweets, testXUsers))
	}))
	defer ts.Close()

	g := &XGatherer{
		Client:      ts.Client(),
		BearerToken: "test-token",
	}
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), []string{"Go"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(items) != 2 {
		t.Fatalf("expected 2 items, got %d", len(items))
	}

	// All items should have "x" tag
	for _, item := range items {
		hasTag := false
		for _, tag := range item.Tags {
			if tag == "x" {
				hasTag = true
				break
			}
		}
		if !hasTag {
			t.Errorf("item %q missing 'x' tag", item.Title)
		}
	}
}

func TestXGatherer_Gather_EmptyTerms(t *testing.T) {
	g := &XGatherer{BearerToken: "test-token"}
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if items != nil {
		t.Errorf("expected nil items for empty search terms, got %d", len(items))
	}
}

func TestXGatherer_Gather_NoBearerToken(t *testing.T) {
	g := &XGatherer{BearerToken: ""}
	items, err := g.Gather(context.Background(), []string{"Go"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if items != nil {
		t.Errorf("expected nil items when no bearer token set, got %d", len(items))
	}
}

func TestXGatherer_Gather_HTTPError_ReturnsError(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
	}))
	defer ts.Close()

	g := &XGatherer{
		Client:      ts.Client(),
		BearerToken: "test-token",
	}
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	_, err := g.Gather(context.Background(), []string{"test"})
	if err == nil {
		t.Fatal("expected error when all X queries fail")
	}
}

func TestXGatherer_Gather_Unauthorized(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusUnauthorized)
	}))
	defer ts.Close()

	g := &XGatherer{
		Client:      ts.Client(),
		BearerToken: "bad-token",
	}
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	_, err := g.Gather(context.Background(), []string{"test"})
	if err == nil {
		t.Fatal("expected error for 401 unauthorized response")
	}
}

func TestXGatherer_Gather_RateLimit(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusTooManyRequests)
	}))
	defer ts.Close()

	g := &XGatherer{
		Client:      ts.Client(),
		BearerToken: "test-token",
	}
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	_, err := g.Gather(context.Background(), []string{"test"})
	if err == nil {
		t.Fatal("expected error for 429 rate limit response")
	}
}

func TestXGatherer_Gather_DeduplicatesAcrossTerms(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write(xTestResponse(testXTweets, testXUsers))
	}))
	defer ts.Close()

	g := &XGatherer{
		Client:      ts.Client(),
		BearerToken: "test-token",
	}
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

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

func TestXGatherer_Gather_Engagement(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write(xTestResponse(testXTweets, testXUsers))
	}))
	defer ts.Close()

	g := &XGatherer{
		Client:      ts.Client(),
		BearerToken: "test-token",
	}
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), []string{"Go"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	// First tweet: 42 likes + 10 retweets + 3 quotes = 55
	for _, item := range items {
		if item.Author == "@golang" && item.Engagement != 55 {
			t.Errorf("expected engagement=55 for @golang tweet, got %d", item.Engagement)
		}
	}
}

func TestXTweetToIntel_ResolvesAuthorName(t *testing.T) {
	authors := map[string]xUser{
		"100": {ID: "100", Name: "Go Team", Username: "golang"},
	}
	tweet := xTweet{
		ID:        "111",
		Text:      "test tweet",
		AuthorID:  "100",
		CreatedAt: "2026-02-17T10:00:00Z",
	}

	item := xTweetToIntel(tweet, authors)
	if item.Author != "@golang" {
		t.Errorf("expected author @golang, got %q", item.Author)
	}
	if item.SourceURL != "https://x.com/golang/status/111" {
		t.Errorf("expected x.com URL with username, got %q", item.SourceURL)
	}
}

func TestXTweetToIntel_FallsBackToUsername(t *testing.T) {
	authors := map[string]xUser{
		"200": {ID: "200", Name: "", Username: "rustlang"},
	}
	tweet := xTweet{
		ID:       "222",
		Text:     "test",
		AuthorID: "200",
	}

	item := xTweetToIntel(tweet, authors)
	// When Name is empty, display author should use @username format in title
	if item.Author != "@rustlang" {
		t.Errorf("expected author @rustlang, got %q", item.Author)
	}
}

func TestXTweetToIntel_UnknownAuthor(t *testing.T) {
	authors := map[string]xUser{}
	tweet := xTweet{
		ID:       "333",
		Text:     "mystery tweet",
		AuthorID: "999",
	}

	item := xTweetToIntel(tweet, authors)
	// Should fall back to author_id
	if item.Author != "999" {
		t.Errorf("expected author_id fallback '999', got %q", item.Author)
	}
	// URL should use /i/ prefix when username unknown
	if item.SourceURL != "https://x.com/i/status/333" {
		t.Errorf("expected fallback URL, got %q", item.SourceURL)
	}
}

func TestXTweetToIntel_TimestampParsing(t *testing.T) {
	authors := map[string]xUser{}
	tweet := xTweet{
		ID:        "444",
		Text:      "test",
		AuthorID:  "1",
		CreatedAt: "2026-03-15T14:30:00Z",
	}

	item := xTweetToIntel(tweet, authors)
	if item.Timestamp.IsZero() {
		t.Error("expected non-zero timestamp")
	}
	if item.Timestamp.Year() != 2026 || item.Timestamp.Month() != 3 || item.Timestamp.Day() != 15 {
		t.Errorf("unexpected timestamp: %v", item.Timestamp)
	}
}

func TestXTweetToIntel_HasXTag(t *testing.T) {
	item := xTweetToIntel(xTweet{ID: "1", Text: "t", AuthorID: "1"}, nil)
	found := false
	for _, tag := range item.Tags {
		if tag == "x" {
			found = true
		}
	}
	if !found {
		t.Error("expected 'x' tag on intel item")
	}
}

func TestTruncateTitle(t *testing.T) {
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
		got := truncateTitle(tc.text, tc.maxLen)
		if got != tc.want {
			t.Errorf("truncateTitle(%q, %d) = %q, want %q", tc.text, tc.maxLen, got, tc.want)
		}
	}
}

func TestXGatherer_Gather_EmptyResponse(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write(xTestResponse(nil, nil))
	}))
	defer ts.Close()

	g := &XGatherer{
		Client:      ts.Client(),
		BearerToken: "test-token",
	}
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), []string{"obscure-query"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 0 {
		t.Errorf("expected 0 items for empty response, got %d", len(items))
	}
}

func TestXGatherer_SearchTweets_SendsAuthHeader(t *testing.T) {
	var gotAuth string
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotAuth = r.Header.Get("Authorization")
		w.Header().Set("Content-Type", "application/json")
		w.Write(xTestResponse(nil, nil))
	}))
	defer ts.Close()

	g := &XGatherer{
		Client:      ts.Client(),
		BearerToken: "my-secret-token",
	}
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	g.Gather(context.Background(), []string{"test"})

	if gotAuth != "Bearer my-secret-token" {
		t.Errorf("expected Authorization header 'Bearer my-secret-token', got %q", gotAuth)
	}
}

func TestXGatherer_SearchTweets_ExcludesRetweets(t *testing.T) {
	var gotQuery string
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotQuery = r.URL.Query().Get("query")
		w.Header().Set("Content-Type", "application/json")
		w.Write(xTestResponse(nil, nil))
	}))
	defer ts.Close()

	g := &XGatherer{
		Client:      ts.Client(),
		BearerToken: "test-token",
	}
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	g.Gather(context.Background(), []string{"golang"})

	if gotQuery != "golang -is:retweet" {
		t.Errorf("expected query with -is:retweet filter, got %q", gotQuery)
	}
}
