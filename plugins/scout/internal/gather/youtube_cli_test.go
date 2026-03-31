package gather

import (
	"os/exec"
	"testing"
	"time"
)

func TestYouTubeCLIGatherer_Name(t *testing.T) {
	g := NewYouTubeCLIGatherer()
	if g.Name() != "youtube-cli" {
		t.Errorf("expected youtube-cli, got %s", g.Name())
	}
}

func TestMapYouTubeScanItems_Empty(t *testing.T) {
	items := mapYouTubeScanItems(nil)
	if len(items) != 0 {
		t.Errorf("expected 0 items for nil input, got %d", len(items))
	}
}

func TestMapYouTubeScanItems_Mapping(t *testing.T) {
	ts := time.Date(2026, 3, 1, 12, 0, 0, 0, time.UTC)
	input := []youtubeScanItem{
		{
			ID:        "abc123",
			Platform:  "youtube",
			URL:       "https://www.youtube.com/watch?v=abc123",
			Title:     "Go Concurrency Patterns",
			Body:      "Deep dive into goroutines",
			Author:    "GoDevChannel",
			CreatedAt: ts,
		},
	}

	items := mapYouTubeScanItems(input)
	if len(items) != 1 {
		t.Fatalf("expected 1 item, got %d", len(items))
	}

	got := items[0]
	if got.ID != "abc123" {
		t.Errorf("ID: expected abc123, got %s", got.ID)
	}
	if got.Title != "Go Concurrency Patterns" {
		t.Errorf("Title: expected Go Concurrency Patterns, got %s", got.Title)
	}
	if got.Content != "Deep dive into goroutines" {
		t.Errorf("Content: expected Deep dive into goroutines, got %s", got.Content)
	}
	if got.SourceURL != "https://www.youtube.com/watch?v=abc123" {
		t.Errorf("SourceURL: expected youtube URL, got %s", got.SourceURL)
	}
	if got.Author != "GoDevChannel" {
		t.Errorf("Author: expected GoDevChannel, got %s", got.Author)
	}
	if !got.Timestamp.Equal(ts) {
		t.Errorf("Timestamp: expected %v, got %v", ts, got.Timestamp)
	}
	if len(got.Tags) != 1 || got.Tags[0] != "youtube-cli" {
		t.Errorf("Tags: expected [youtube-cli], got %v", got.Tags)
	}
	if got.Engagement != 0 {
		t.Errorf("Engagement: expected 0, got %d", got.Engagement)
	}
}

func TestMapYouTubeScanItems_Deduplicates(t *testing.T) {
	input := []youtubeScanItem{
		{ID: "dup1", Title: "Video A", URL: "https://youtube.com/watch?v=dup1"},
		{ID: "dup1", Title: "Video A duplicate", URL: "https://youtube.com/watch?v=dup1"},
	}

	items := mapYouTubeScanItems(input)
	if len(items) != 1 {
		t.Errorf("expected 1 item after dedup, got %d", len(items))
	}
}

func TestMapYouTubeScanItems_FallbackIDFromURL(t *testing.T) {
	input := []youtubeScanItem{
		{ID: "", Title: "Video A", URL: "https://youtube.com/watch?v=someid"},
	}

	items := mapYouTubeScanItems(input)
	if len(items) != 1 {
		t.Fatalf("expected 1 item, got %d", len(items))
	}
	if items[0].ID == "" {
		t.Error("expected non-empty ID generated from URL when ID field is blank")
	}
}

func TestMapYouTubeScanItems_ZeroTimestampFallsBackToNow(t *testing.T) {
	before := time.Now().UTC()
	input := []youtubeScanItem{
		{ID: "ts1", Title: "Video", URL: "https://youtube.com/watch?v=ts1"},
	}

	items := mapYouTubeScanItems(input)
	after := time.Now().UTC()

	if len(items) != 1 {
		t.Fatalf("expected 1 item, got %d", len(items))
	}
	if items[0].Timestamp.Before(before) || items[0].Timestamp.After(after) {
		t.Errorf("expected Timestamp to be near now, got %v", items[0].Timestamp)
	}
}

func TestYouTubeCLIGatherer_NotInstalled(t *testing.T) {
	if _, err := exec.LookPath("youtube"); err == nil {
		t.Skip("youtube CLI is installed; cannot test not-installed path")
	}

	g := NewYouTubeCLIGatherer()
	items, err := g.Gather(t.Context(), nil)
	if err != nil {
		t.Errorf("expected nil error when youtube CLI not installed, got: %v", err)
	}
	if items != nil {
		t.Errorf("expected nil items when youtube CLI not installed, got %d", len(items))
	}
}

func TestYouTubeCLIGatherer_RegistryEntry(t *testing.T) {
	factory, ok := Registry["youtube-cli"]
	if !ok {
		t.Fatal("youtube-cli not found in Registry")
	}

	g, err := factory(TopicConfig{})
	if err != nil {
		t.Fatalf("factory returned error: %v", err)
	}
	if g.Name() != "youtube-cli" {
		t.Errorf("expected youtube-cli gatherer name, got %s", g.Name())
	}
}
