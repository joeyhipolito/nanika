package cmd

import (
	"testing"
	"time"

	"github.com/joeyhipolito/nanika-linkedin/internal/api"
)

func TestToPostItem(t *testing.T) {
	ts := time.Date(2025, 6, 15, 10, 30, 0, 0, time.UTC).UnixMilli()
	p := api.Post{
		ID:             "urn:li:share:123456",
		Commentary:     "Hello LinkedIn!",
		Visibility:     "PUBLIC",
		LifecycleState: "PUBLISHED",
		CreatedAt:      ts,
	}

	item := toPostItem(p)

	if item.ID != "urn:li:share:123456" {
		t.Errorf("ID = %q, want %q", item.ID, "urn:li:share:123456")
	}
	if item.Commentary != "Hello LinkedIn!" {
		t.Errorf("Commentary = %q, want %q", item.Commentary, "Hello LinkedIn!")
	}
	if item.Visibility != "PUBLIC" {
		t.Errorf("Visibility = %q, want %q", item.Visibility, "PUBLIC")
	}
	if item.State != "PUBLISHED" {
		t.Errorf("State = %q, want %q", item.State, "PUBLISHED")
	}
	if item.URL != "https://www.linkedin.com/feed/update/urn:li:share:123456" {
		t.Errorf("URL = %q, want https://www.linkedin.com/feed/update/urn:li:share:123456", item.URL)
	}
	if item.CreatedAt == "" {
		t.Error("CreatedAt should not be empty")
	}
}

func TestToPostItemZeroTimestamp(t *testing.T) {
	p := api.Post{
		ID:         "urn:li:share:999",
		CreatedAt:  0,
	}
	item := toPostItem(p)
	if item.CreatedAt != "" {
		t.Errorf("CreatedAt should be empty for zero timestamp, got %q", item.CreatedAt)
	}
}
