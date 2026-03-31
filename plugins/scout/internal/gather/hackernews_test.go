package gather

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

func TestHackerNewsGatherer_Gather_HappyPath(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		resp := hnSearchResponse{
			Hits: []hnHit{
				{
					ObjectID:    "12345",
					Title:       "Show HN: A new Go CLI framework",
					URL:         "https://example.com/go-cli",
					Author:      "testuser",
					Points:      150,
					NumComments: 42,
					StoryText:   "",
					CreatedAtI:  time.Now().Unix(),
					Tags:        []string{"story", "show_hn", "author_testuser"},
				},
				{
					ObjectID:    "12346",
					Title:       "Ask HN: Best Go testing practices?",
					URL:         "",
					Author:      "another",
					Points:      80,
					NumComments: 25,
					StoryText:   "What are your favorite Go testing patterns?",
					CreatedAtI:  time.Now().Add(-24 * time.Hour).Unix(),
					Tags:        []string{"story", "ask_hn"},
				},
			},
		}
		json.NewEncoder(w).Encode(resp)
	}))
	defer ts.Close()

	g := NewHackerNewsGatherer()
	g.Client = ts.Client()

	// Override the search URL by intercepting — use a transport that redirects to test server
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), []string{"Go", "CLI"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(items) == 0 {
		t.Fatal("expected items, got none")
	}

	// Verify first item has external URL
	found := false
	for _, item := range items {
		if item.Title == "Show HN: A new Go CLI framework" {
			found = true
			if item.SourceURL != "https://example.com/go-cli" {
				t.Errorf("expected external URL, got %s", item.SourceURL)
			}
			if item.Engagement != 192 { // 150 + 42
				t.Errorf("expected engagement 192, got %d", item.Engagement)
			}
			if item.Author != "testuser" {
				t.Errorf("expected author testuser, got %s", item.Author)
			}
			break
		}
	}
	if !found {
		t.Error("expected to find 'Show HN: A new Go CLI framework' in results")
	}
}

func TestHackerNewsGatherer_Gather_EmptyTerms(t *testing.T) {
	g := NewHackerNewsGatherer()
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if items != nil {
		t.Errorf("expected nil items for empty terms, got %d", len(items))
	}
}

func TestHackerNewsGatherer_Gather_APIError(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
	}))
	defer ts.Close()

	g := NewHackerNewsGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	_, err := g.Gather(context.Background(), []string{"test"})
	if err == nil {
		t.Fatal("expected error for 500 response")
	}
}

func TestHackerNewsGatherer_Gather_RateLimit(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusTooManyRequests)
	}))
	defer ts.Close()

	g := NewHackerNewsGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	_, err := g.Gather(context.Background(), []string{"test"})
	if err == nil {
		t.Fatal("expected error for 429 response")
	}
}

func TestHnHitToIntel_AskHN_FallbackURL(t *testing.T) {
	hit := hnHit{
		ObjectID:    "99999",
		Title:       "Ask HN: What are you working on?",
		URL:         "", // No external URL
		Author:      "someone",
		Points:      50,
		NumComments: 100,
		CreatedAtI:  time.Now().Unix(),
	}

	item := hnHitToIntel(hit)

	expected := "https://news.ycombinator.com/item?id=99999"
	if item.SourceURL != expected {
		t.Errorf("expected fallback URL %s, got %s", expected, item.SourceURL)
	}
}

func TestHnHitToIntel_TagFiltering(t *testing.T) {
	hit := hnHit{
		ObjectID: "1",
		Title:    "Test",
		Author:   "bob",
		Tags:     []string{"story", "author_bob", "show_hn"},
	}

	item := hnHitToIntel(hit)

	// "story" and "author_bob" should be filtered out
	for _, tag := range item.Tags {
		if tag == "story" || tag == "author_bob" {
			t.Errorf("expected tag %q to be filtered out", tag)
		}
	}
	// "show_hn" and "hackernews" should be present
	hasShowHN := false
	hasHN := false
	for _, tag := range item.Tags {
		if tag == "show_hn" {
			hasShowHN = true
		}
		if tag == "hackernews" {
			hasHN = true
		}
	}
	if !hasShowHN {
		t.Error("expected show_hn tag")
	}
	if !hasHN {
		t.Error("expected hackernews tag")
	}
}

func TestParseHNResponse_ValidJSON(t *testing.T) {
	data := []byte(`{
		"hits": [
			{
				"objectID": "1",
				"title": "Test Story",
				"url": "https://example.com",
				"author": "test",
				"points": 100,
				"num_comments": 50,
				"created_at_i": 1700000000,
				"_tags": ["story"]
			}
		]
	}`)

	items, err := ParseHNResponse(data)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 1 {
		t.Fatalf("expected 1 item, got %d", len(items))
	}
	if items[0].Title != "Test Story" {
		t.Errorf("expected title 'Test Story', got %s", items[0].Title)
	}
	if items[0].Engagement != 150 {
		t.Errorf("expected engagement 150, got %d", items[0].Engagement)
	}
}

func TestParseHNResponse_InvalidJSON(t *testing.T) {
	_, err := ParseHNResponse([]byte(`{invalid`))
	if err == nil {
		t.Fatal("expected error for invalid JSON")
	}
}

func TestParseHNResponse_EmptyHits(t *testing.T) {
	data := []byte(`{"hits": []}`)
	items, err := ParseHNResponse(data)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 0 {
		t.Errorf("expected 0 items, got %d", len(items))
	}
}

func TestHnHitToIntel_ContentTruncation(t *testing.T) {
	longText := ""
	for i := 0; i < 600; i++ {
		longText += "x"
	}

	hit := hnHit{
		ObjectID:  "1",
		Title:     "Test",
		StoryText: longText,
	}

	item := hnHitToIntel(hit)
	if len(item.Content) > 504 { // 500 + "..."
		t.Errorf("expected content truncated to ~503 chars, got %d", len(item.Content))
	}
}

// rewriteTransport redirects all requests to a test server URL.
type rewriteTransport struct {
	URL string
}

func (t *rewriteTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	req.URL.Scheme = "http"
	req.URL.Host = t.URL[len("http://"):]
	return http.DefaultTransport.RoundTrip(req)
}
