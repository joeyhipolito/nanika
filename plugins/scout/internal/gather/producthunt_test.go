package gather

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

func testPHResponse() phGraphQLResponse {
	return phGraphQLResponse{
		Data: struct {
			Posts struct {
				Edges []struct {
					Node phPost `json:"node"`
				} `json:"edges"`
			} `json:"posts"`
		}{
			Posts: struct {
				Edges []struct {
					Node phPost `json:"node"`
				} `json:"edges"`
			}{
				Edges: []struct {
					Node phPost `json:"node"`
				}{
					{
						Node: phPost{
							ID:            "123",
							Name:          "DevTool Pro",
							Tagline:       "The best developer tool for Go programmers",
							URL:           "https://www.producthunt.com/posts/devtool-pro",
							VotesCount:    250,
							CommentsCount: 30,
							CreatedAt:     time.Now().UTC().Format(time.RFC3339),
							Topics: struct {
								Edges []struct {
									Node struct {
										Name string `json:"name"`
									} `json:"node"`
								} `json:"edges"`
							}{
								Edges: []struct {
									Node struct {
										Name string `json:"name"`
									} `json:"node"`
								}{
									{Node: struct {
										Name string `json:"name"`
									}{Name: "Developer Tools"}},
								},
							},
							Makers: []struct {
								Name string `json:"name"`
							}{
								{Name: "Jane Doe"},
							},
						},
					},
					{
						Node: phPost{
							ID:            "456",
							Name:          "AI Writer",
							Tagline:       "AI-powered writing assistant for technical docs",
							URL:           "https://www.producthunt.com/posts/ai-writer",
							VotesCount:    180,
							CommentsCount: 15,
							CreatedAt:     time.Now().Add(-24 * time.Hour).UTC().Format(time.RFC3339),
							Topics: struct {
								Edges []struct {
									Node struct {
										Name string `json:"name"`
									} `json:"node"`
								} `json:"edges"`
							}{
								Edges: []struct {
									Node struct {
										Name string `json:"name"`
									} `json:"node"`
								}{
									{Node: struct {
										Name string `json:"name"`
									}{Name: "Artificial Intelligence"}},
								},
							},
							Makers: []struct {
								Name string `json:"name"`
							}{
								{Name: "Bob Smith"},
							},
						},
					},
				},
			},
		},
	}
}

func TestProductHuntGatherer_Gather_HappyPath(t *testing.T) {
	resp := testPHResponse()
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != "POST" {
			t.Errorf("expected POST, got %s", r.Method)
		}
		if r.Header.Get("Authorization") != "Bearer test-token" {
			t.Errorf("expected auth header, got %s", r.Header.Get("Authorization"))
		}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(resp)
	}))
	defer ts.Close()

	g := NewProductHuntGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	t.Setenv("PRODUCTHUNT_TOKEN", "test-token")

	items, err := g.Gather(context.Background(), []string{"developer", "tool"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(items) == 0 {
		t.Fatal("expected items, got none")
	}

	// Verify first item
	found := false
	for _, item := range items {
		if item.Title == "DevTool Pro" {
			found = true
			if item.SourceURL != "https://www.producthunt.com/posts/devtool-pro" {
				t.Errorf("expected PH URL, got %s", item.SourceURL)
			}
			if item.Engagement != 280 { // 250 + 30
				t.Errorf("expected engagement 280, got %d", item.Engagement)
			}
			if item.Author != "Jane Doe" {
				t.Errorf("expected author Jane Doe, got %s", item.Author)
			}
			hasPHTag := false
			hasTopicTag := false
			for _, tag := range item.Tags {
				if tag == "producthunt" {
					hasPHTag = true
				}
				if tag == "developer tools" {
					hasTopicTag = true
				}
			}
			if !hasPHTag {
				t.Error("expected producthunt tag")
			}
			if !hasTopicTag {
				t.Error("expected 'developer tools' topic tag")
			}
			break
		}
	}
	if !found {
		t.Error("expected to find 'DevTool Pro' in results")
	}
}

func TestProductHuntGatherer_Gather_NoToken(t *testing.T) {
	t.Setenv("PRODUCTHUNT_TOKEN", "")

	g := NewProductHuntGatherer()
	items, err := g.Gather(context.Background(), []string{"test"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if items != nil {
		t.Errorf("expected nil items when no token, got %d", len(items))
	}
}

func TestProductHuntGatherer_Gather_APIError(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
	}))
	defer ts.Close()

	g := NewProductHuntGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	t.Setenv("PRODUCTHUNT_TOKEN", "test-token")

	_, err := g.Gather(context.Background(), []string{"test"})
	if err == nil {
		t.Fatal("expected error for 500 response")
	}
}

func TestProductHuntGatherer_Gather_Unauthorized(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusUnauthorized)
	}))
	defer ts.Close()

	g := NewProductHuntGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	t.Setenv("PRODUCTHUNT_TOKEN", "bad-token")

	_, err := g.Gather(context.Background(), []string{"test"})
	if err == nil {
		t.Fatal("expected error for 401 response")
	}
}

func TestProductHuntGatherer_Gather_RateLimit(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusTooManyRequests)
	}))
	defer ts.Close()

	g := NewProductHuntGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	t.Setenv("PRODUCTHUNT_TOKEN", "test-token")

	_, err := g.Gather(context.Background(), []string{"test"})
	if err == nil {
		t.Fatal("expected error for 429 response")
	}
}

func TestProductHuntGatherer_Gather_GraphQLError(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write([]byte(`{"data":null,"errors":[{"message":"Invalid query"}]}`))
	}))
	defer ts.Close()

	g := NewProductHuntGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	t.Setenv("PRODUCTHUNT_TOKEN", "test-token")

	_, err := g.Gather(context.Background(), []string{"test"})
	if err == nil {
		t.Fatal("expected error for GraphQL error response")
	}
}

func TestProductHuntGatherer_Gather_FiltersByTerms(t *testing.T) {
	resp := testPHResponse()
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(resp)
	}))
	defer ts.Close()

	g := NewProductHuntGatherer()
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	t.Setenv("PRODUCTHUNT_TOKEN", "test-token")

	// Search for "AI" should only match "AI Writer"
	items, err := g.Gather(context.Background(), []string{"AI"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	for _, item := range items {
		if item.Title == "DevTool Pro" {
			t.Error("DevTool Pro should have been filtered out by 'AI' search term")
		}
	}
}

func TestPhPostToIntel_NoMakers(t *testing.T) {
	post := phPost{
		ID:      "999",
		Name:    "Orphan Product",
		Tagline: "No makers listed",
		URL:     "https://www.producthunt.com/posts/orphan",
	}

	item := phPostToIntel(post)
	if item.Author != "" {
		t.Errorf("expected empty author, got %s", item.Author)
	}
}

func TestPhPostToIntel_ContentTruncation(t *testing.T) {
	longTagline := ""
	for i := 0; i < 600; i++ {
		longTagline += "x"
	}

	post := phPost{
		ID:      "1",
		Name:    "Test",
		Tagline: longTagline,
		URL:     "https://www.producthunt.com/posts/test",
	}

	item := phPostToIntel(post)
	if len(item.Content) > 504 { // 500 + "..."
		t.Errorf("expected content truncated to ~503 chars, got %d", len(item.Content))
	}
}

func TestParseProductHuntResponse_ValidJSON(t *testing.T) {
	data := []byte(`{
		"data": {
			"posts": {
				"edges": [
					{
						"node": {
							"id": "1",
							"name": "Test Product",
							"tagline": "A test product",
							"url": "https://www.producthunt.com/posts/test-product",
							"votesCount": 100,
							"commentsCount": 20,
							"createdAt": "2026-03-17T00:00:00Z",
							"topics": {"edges": []},
							"makers": [{"name": "Test Maker"}]
						}
					}
				]
			}
		}
	}`)

	items, err := ParseProductHuntResponse(data)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 1 {
		t.Fatalf("expected 1 item, got %d", len(items))
	}
	if items[0].Title != "Test Product" {
		t.Errorf("expected title 'Test Product', got %s", items[0].Title)
	}
	if items[0].Engagement != 120 {
		t.Errorf("expected engagement 120, got %d", items[0].Engagement)
	}
	if items[0].Author != "Test Maker" {
		t.Errorf("expected author 'Test Maker', got %s", items[0].Author)
	}
}

func TestParseProductHuntResponse_InvalidJSON(t *testing.T) {
	_, err := ParseProductHuntResponse([]byte(`{invalid`))
	if err == nil {
		t.Fatal("expected error for invalid JSON")
	}
}

func TestParseProductHuntResponse_EmptyEdges(t *testing.T) {
	data := []byte(`{"data":{"posts":{"edges":[]}}}`)
	items, err := ParseProductHuntResponse(data)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 0 {
		t.Errorf("expected 0 items, got %d", len(items))
	}
}
