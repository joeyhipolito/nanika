package gather

import (
	"context"
	"testing"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/browser"
)

// skipIfChromeUnavailable checks localhost:9222 and skips the test if Chrome is not running.
func skipIfChromeUnavailable(t *testing.T) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	client, err := browser.New(ctx, browser.DefaultCDPURL)
	if err != nil {
		t.Skipf("Chrome not available at localhost:9222 (%v)", err)
	}
	client.Close()
}

func TestGoogleBrowserGatherer_Live(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping live browser test in short mode")
	}
	skipIfChromeUnavailable(t)

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	g := NewGoogleBrowserGatherer()
	items, err := g.Gather(ctx, nil)
	if err != nil {
		t.Fatalf("gather error: %v", err)
	}

	t.Logf("google-browser: got %d items", len(items))
	for i, item := range items {
		if i >= 3 {
			break
		}
		t.Logf("  [%d] %s — %s", i+1, item.Title, item.SourceURL)
	}

	// If signed in, we expect items. If not, the feed is empty but that's not an error.
	for _, item := range items {
		if item.ID == "" {
			t.Error("item missing ID")
		}
		if item.Title == "" {
			t.Error("item missing title")
		}
		if item.SourceURL == "" {
			t.Error("item missing source URL")
		}
	}
}

func TestLinkedInBrowserGatherer_Live(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping live browser test in short mode")
	}
	skipIfChromeUnavailable(t)

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	g := NewLinkedInBrowserGatherer()
	items, err := g.Gather(ctx, nil)
	if err != nil {
		t.Fatalf("gather error: %v", err)
	}

	t.Logf("linkedin-browser: got %d items", len(items))
	for i, item := range items {
		if i >= 3 {
			break
		}
		t.Logf("  [%d] %s — %s", i+1, item.Title, item.SourceURL)
	}

	for _, item := range items {
		if item.ID == "" {
			t.Error("item missing ID")
		}
		if item.Title == "" {
			t.Error("item missing title")
		}
	}
}

func TestSubstackBrowserGatherer_Live(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping live browser test in short mode")
	}
	skipIfChromeUnavailable(t)

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	g := NewSubstackBrowserGatherer()
	items, err := g.Gather(ctx, nil)
	if err != nil {
		t.Fatalf("gather error: %v", err)
	}

	t.Logf("substack-browser: got %d items", len(items))
	for i, item := range items {
		if i >= 3 {
			break
		}
		t.Logf("  [%d] %s — %s", i+1, item.Title, item.SourceURL)
	}

	for _, item := range items {
		if item.ID == "" {
			t.Error("item missing ID")
		}
		if item.Title == "" {
			t.Error("item missing title")
		}
	}
}

func TestXBrowserGatherer_Live(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping live browser test in short mode")
	}
	skipIfChromeUnavailable(t)

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	g := NewXBrowserGatherer()
	items, err := g.Gather(ctx, nil)
	if err != nil {
		t.Fatalf("gather error: %v", err)
	}

	t.Logf("x-browser: got %d items", len(items))
	for i, item := range items {
		if i >= 3 {
			break
		}
		t.Logf("  [%d] %s — %s", i+1, item.Title, item.SourceURL)
	}

	for _, item := range items {
		if item.ID == "" {
			t.Error("item missing ID")
		}
		if item.Title == "" {
			t.Error("item missing title")
		}
	}
}

// TestBrowserGatherer_ChromeUnavailable verifies all 4 gatherers return (nil, nil)
// when Chrome is unreachable, without returning an error.
func TestBrowserGatherer_ChromeUnavailable(t *testing.T) {
	ctx := context.Background()

	gatherers := []Gatherer{
		NewGoogleBrowserGatherer(),
		NewLinkedInBrowserGatherer(),
		NewSubstackBrowserGatherer(),
		NewXBrowserGatherer(),
	}

	// Point at a port that should have nothing listening.
	// We can't easily override the CDP URL in the current design, so we test
	// that the gatherer returns gracefully by running against a known-bad address.
	// This test relies on the gatherer already having Chrome unavailable handling.
	// If Chrome IS running, skip — we can't fake unavailability.
	checkCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()
	client, err := browser.New(checkCtx, browser.DefaultCDPURL)
	if err == nil {
		client.Close()
		t.Skip("Chrome is running; cannot test Chrome-unavailable path")
	}

	for _, g := range gatherers {
		items, gErr := g.Gather(ctx, nil)
		if gErr != nil {
			t.Errorf("%s: expected nil error when Chrome unavailable, got: %v", g.Name(), gErr)
		}
		if items != nil {
			t.Errorf("%s: expected nil items when Chrome unavailable, got %d", g.Name(), len(items))
		}
	}
}
