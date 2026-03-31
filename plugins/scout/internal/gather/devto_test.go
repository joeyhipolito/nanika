package gather

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

func newDevToTestArticles() []devtoArticle {
	return []devtoArticle{
		{
			ID:                     1001,
			Title:                  "Building CLI Tools in Go",
			Description:            "A guide to building production-ready CLI tools",
			URL:                    "https://dev.to/testuser/building-cli-tools-in-go",
			PublishedAt:            "2026-02-17T10:00:00Z",
			User:                   devtoUser{Name: "Test User", Username: "testuser"},
			Tags:                   "go, cli, tutorial",
			PositiveReactionsCount: 42,
			CommentsCount:          8,
		},
		{
			ID:                     1002,
			Title:                  "Understanding Go Concurrency",
			Description:            "Deep dive into goroutines and channels",
			URL:                    "https://dev.to/another/go-concurrency",
			PublishedAt:            "2026-02-16T08:00:00Z",
			User:                   devtoUser{Name: "", Username: "another"},
			Tags:                   "go, concurrency",
			PositiveReactionsCount: 100,
			CommentsCount:          20,
		},
	}
}

func TestDevToGatherer_Gather_HappyPath(t *testing.T) {
	articles := newDevToTestArticles()
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(articles)
	}))
	defer ts.Close()

	g := NewDevToGatherer([]string{"go"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), []string{"Go", "CLI"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(items) == 0 {
		t.Fatal("expected items, got none")
	}

	// Verify first item
	found := false
	for _, item := range items {
		if item.Title == "Building CLI Tools in Go" {
			found = true
			if item.Engagement != 50 { // 42 + 8
				t.Errorf("expected engagement 50, got %d", item.Engagement)
			}
			if item.Author != "Test User" {
				t.Errorf("expected author 'Test User', got %s", item.Author)
			}
			break
		}
	}
	if !found {
		t.Error("expected to find 'Building CLI Tools in Go'")
	}
}

func TestDevToGatherer_Gather_FallbackAuthorUsername(t *testing.T) {
	articles := []devtoArticle{
		{
			ID:          2001,
			Title:       "Test Article",
			URL:         "https://dev.to/test",
			User:        devtoUser{Name: "", Username: "fallback_user"},
			Tags:        "test",
			PublishedAt: "2026-02-17T10:00:00Z",
		},
	}

	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(articles)
	}))
	defer ts.Close()

	g := NewDevToGatherer([]string{"test"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), []string{"Test"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(items) == 0 {
		t.Fatal("expected items")
	}

	if items[0].Author != "fallback_user" {
		t.Errorf("expected author fallback to username 'fallback_user', got %s", items[0].Author)
	}
}

func TestDevToGatherer_Gather_EmptyTagsAndTerms(t *testing.T) {
	g := NewDevToGatherer(nil)
	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 0 {
		t.Errorf("expected 0 items for no tags/terms, got %d", len(items))
	}
}

func TestDevToGatherer_FetchDevTo_RetryOn429(t *testing.T) {
	callCount := 0
	articles := newDevToTestArticles()
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		callCount++
		if callCount == 1 {
			w.WriteHeader(http.StatusTooManyRequests)
			return
		}
		json.NewEncoder(w).Encode(articles)
	}))
	defer ts.Close()

	g := NewDevToGatherer(nil)
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.fetchDevTo(context.Background(), ts.URL + "/api/articles?tag=go&per_page=25")
	if err != nil {
		t.Fatalf("expected retry to succeed, got: %v", err)
	}
	if len(items) != len(articles) {
		t.Errorf("expected %d items after retry, got %d", len(articles), len(items))
	}
	if callCount != 2 {
		t.Errorf("expected 2 calls (1 retry), got %d", callCount)
	}
}

func TestDevToGatherer_FetchDevTo_ExhaustsRetries(t *testing.T) {
	callCount := 0
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		callCount++
		w.WriteHeader(http.StatusTooManyRequests)
	}))
	defer ts.Close()

	g := NewDevToGatherer(nil)
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	_, err := g.fetchDevTo(context.Background(), ts.URL + "/api/articles?tag=go")
	if err == nil {
		t.Fatal("expected error after exhausting retries")
	}
	if callCount != 3 { // initial + 2 retries
		t.Errorf("expected 3 calls (initial + 2 retries), got %d", callCount)
	}
}

func TestDevToGatherer_Gather_TagsInResult(t *testing.T) {
	articles := newDevToTestArticles()
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(articles)
	}))
	defer ts.Close()

	g := NewDevToGatherer([]string{"go"})
	g.Client = ts.Client()
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), []string{"Go"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	for _, item := range items {
		hasDevTo := false
		for _, tag := range item.Tags {
			if tag == "devto" {
				hasDevTo = true
				break
			}
		}
		if !hasDevTo {
			t.Errorf("item %q missing devto tag", item.Title)
		}
	}
}

func TestParseDevToResponse_ValidJSON(t *testing.T) {
	data := []byte(`[
		{
			"id": 1,
			"title": "Test Article",
			"description": "A test",
			"url": "https://dev.to/test",
			"published_at": "2026-02-17T10:00:00Z",
			"user": {"name": "Test", "username": "test"},
			"tags": "go, test",
			"positive_reactions_count": 10,
			"comments_count": 5
		}
	]`)

	items, err := ParseDevToResponse(data)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 1 {
		t.Fatalf("expected 1 item, got %d", len(items))
	}
	if items[0].Engagement != 15 {
		t.Errorf("expected engagement 15, got %d", items[0].Engagement)
	}
}

func TestParseDevToResponse_InvalidJSON(t *testing.T) {
	_, err := ParseDevToResponse([]byte(`{invalid`))
	if err == nil {
		t.Fatal("expected error for invalid JSON")
	}
}

func TestParseDevToResponse_EmptyArray(t *testing.T) {
	items, err := ParseDevToResponse([]byte(`[]`))
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 0 {
		t.Errorf("expected 0 items, got %d", len(items))
	}
}

func TestDevtoArticleToIntel_TagParsing(t *testing.T) {
	article := devtoArticle{
		ID:    1,
		Title: "Test",
		URL:   "https://dev.to/test",
		Tags:  "go, cli, tutorial",
		User:  devtoUser{Name: "Test", Username: "test"},
	}

	item := devtoArticleToIntel(article)

	// Should have go, cli, tutorial, devto
	if len(item.Tags) != 4 {
		t.Errorf("expected 4 tags, got %d: %v", len(item.Tags), item.Tags)
	}
}
