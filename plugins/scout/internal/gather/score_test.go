package gather

import (
	"math"
	"testing"
	"time"
)

func TestScoreItem_FullRelevanceRecentWithEngagement(t *testing.T) {
	now := time.Now().UTC()
	item := IntelItem{
		Title:      "GPT-5 released by OpenAI",
		Content:    "New LLM foundation model with Claude-beating benchmarks",
		Timestamp:  now.Add(-1 * time.Hour),
		Engagement: 500,
	}
	terms := []string{"GPT", "LLM", "foundation model"}

	ScoreItem(&item, terms, now)

	// All terms match, very recent, good engagement → high score
	if item.Score < 80 {
		t.Errorf("expected score >= 80 for fully relevant recent item with engagement, got %.1f", item.Score)
	}
}

func TestScoreItem_NoEngagement_NotCappedAt70(t *testing.T) {
	now := time.Now().UTC()
	item := IntelItem{
		Title:      "GPT-5 released by OpenAI with new LLM capabilities",
		Content:    "A new foundation model pushing benchmarks",
		Timestamp:  now.Add(-30 * time.Minute),
		Engagement: 0, // RSS source — no engagement data
	}
	terms := []string{"GPT", "LLM", "foundation model"}

	ScoreItem(&item, terms, now)

	// With redistributed weights, a perfectly relevant + fresh item should exceed 70
	if item.Score <= 70 {
		t.Errorf("expected score > 70 for fully relevant recent item without engagement (redistributed weights), got %.1f", item.Score)
	}
}

func TestScoreItem_NoEngagement_WeightsRedistributed(t *testing.T) {
	now := time.Now().UTC()
	item := IntelItem{
		Title:      "Perfect match for all terms here: GPT LLM foundation model",
		Timestamp:  now, // Just now
		Engagement: 0,
	}
	terms := []string{"GPT", "LLM", "foundation model"}

	ScoreItem(&item, terms, now)

	// relevance=1.0, recency=1.0, no engagement → (1.0*0.57 + 1.0*0.43) * 100 = 100
	if item.Score != 100.0 {
		t.Errorf("expected score 100.0 for perfect match without engagement, got %.1f", item.Score)
	}
}

func TestScoreItem_WithEngagement_UsesStandardWeights(t *testing.T) {
	now := time.Now().UTC()
	item := IntelItem{
		Title:      "GPT LLM foundation model",
		Timestamp:  now,
		Engagement: 10000, // maxes out engagement component
	}
	terms := []string{"GPT", "LLM", "foundation model"}

	ScoreItem(&item, terms, now)

	// relevance=1.0, recency=1.0, engagement=1.0 → (0.4 + 0.3 + 0.3) * 100 = 100
	if item.Score != 100.0 {
		t.Errorf("expected score 100.0 for perfect match with max engagement, got %.1f", item.Score)
	}
}

func TestScoreItem_ZeroRelevance(t *testing.T) {
	now := time.Now().UTC()
	item := IntelItem{
		Title:     "Completely unrelated article about cooking",
		Timestamp: now,
	}
	terms := []string{"GPT", "LLM"}

	ScoreItem(&item, terms, now)

	// No terms match, no engagement → only recency contributes
	if item.Score > 50 {
		t.Errorf("expected low score for zero relevance, got %.1f", item.Score)
	}
}

func TestScoreItem_OldItem(t *testing.T) {
	now := time.Now().UTC()
	item := IntelItem{
		Title:     "GPT news from long ago",
		Timestamp: now.Add(-31 * 24 * time.Hour), // 31 days old → recency = 0
	}
	terms := []string{"GPT"}

	ScoreItem(&item, terms, now)

	// Only relevance contributes (1/1 term match)
	// No engagement, recency=0 → score = 0.57 * 100 * (1/1) = 57
	if item.Score > 60 {
		t.Errorf("expected score <= 60 for old item, got %.1f", item.Score)
	}
}

func TestScoreItem_NoSearchTerms(t *testing.T) {
	now := time.Now().UTC()
	item := IntelItem{
		Title:     "Anything",
		Timestamp: now,
	}

	ScoreItem(&item, nil, now)

	// No terms → relevance=1.0, recency=1.0, no engagement → 100
	if item.Score != 100.0 {
		t.Errorf("expected score 100.0 with no search terms, got %.1f", item.Score)
	}
}

func TestScoreItems_ScoresAllItems(t *testing.T) {
	now := time.Now().UTC()
	items := []IntelItem{
		{Title: "GPT article", Timestamp: now},
		{Title: "Another GPT piece", Timestamp: now.Add(-48 * time.Hour)},
	}
	terms := []string{"GPT"}

	result := ScoreItems(items, terms, now)
	if len(result) != 2 {
		t.Fatalf("expected 2 items, got %d", len(result))
	}
	// First item should score higher (more recent)
	if result[0].Score <= result[1].Score {
		t.Errorf("expected first item (newer) to score higher: %.1f vs %.1f", result[0].Score, result[1].Score)
	}
}

func TestCountTermMatches(t *testing.T) {
	item := IntelItem{
		Title:   "Building a CLI tool in Go",
		Content: "This Go library makes CLI development fast",
		Author:  "gopher",
	}

	tests := []struct {
		terms    []string
		expected int
	}{
		{[]string{"Go", "CLI"}, 2},
		{[]string{"Go", "Python"}, 1},
		{[]string{"Rust", "Python"}, 0},
		{[]string{"gopher"}, 1},
	}

	for _, tc := range tests {
		got := countTermMatches(item, tc.terms)
		if got != tc.expected {
			t.Errorf("countTermMatches(%v) = %d, want %d", tc.terms, got, tc.expected)
		}
	}
}

func TestScoreItem_EngagementLogScale(t *testing.T) {
	now := time.Now().UTC()

	// Compare items with different engagement levels
	low := IntelItem{Title: "test", Timestamp: now, Engagement: 10}
	high := IntelItem{Title: "test", Timestamp: now, Engagement: 1000}
	max := IntelItem{Title: "test", Timestamp: now, Engagement: 10000}

	ScoreItem(&low, nil, now)
	ScoreItem(&high, nil, now)
	ScoreItem(&max, nil, now)

	if low.Score >= high.Score {
		t.Errorf("expected higher engagement to give higher score: %v vs %v", low.Score, high.Score)
	}
	if high.Score >= max.Score {
		t.Errorf("expected higher engagement to give higher score: %v vs %v", high.Score, max.Score)
	}
	// Engagement of 10000 should max out the engagement component
	if math.Abs(max.Score-100.0) > 0.2 {
		t.Errorf("expected score ~100 for max engagement with no terms, got %.1f", max.Score)
	}
}
