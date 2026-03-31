package gather

import (
	"testing"
	"time"
)

func TestDedupeItems_ExactIDMatch(t *testing.T) {
	items := []IntelItem{
		{ID: "abc123", Title: "First article", Score: 80},
		{ID: "abc123", Title: "Duplicate by ID", Score: 90},
	}

	result := DedupeItems(items)
	if len(result) != 1 {
		t.Fatalf("expected 1 item after dedup, got %d", len(result))
	}
	// Higher-scored item should be kept
	if result[0].Score != 90 {
		t.Errorf("expected higher-scored item kept, got score %.1f", result[0].Score)
	}
}

func TestDedupeItems_JaccardTitleSimilarity(t *testing.T) {
	items := []IntelItem{
		{ID: "aaa", Title: "OpenAI releases GPT-5 with new capabilities", Score: 70},
		{ID: "bbb", Title: "OpenAI releases GPT-5 with new features", Score: 85},
	}

	result := DedupeItems(items)
	if len(result) != 1 {
		t.Fatalf("expected 1 item after title similarity dedup, got %d", len(result))
	}
	if result[0].Score != 85 {
		t.Errorf("expected higher-scored item kept, got score %.1f", result[0].Score)
	}
}

func TestDedupeItems_URLNormalization(t *testing.T) {
	items := []IntelItem{
		{ID: "aaa", Title: "Article one", SourceURL: "https://www.example.com/post?utm_source=twitter", Score: 60},
		{ID: "bbb", Title: "Different title", SourceURL: "http://example.com/post", Score: 80},
	}

	result := DedupeItems(items)
	if len(result) != 1 {
		t.Fatalf("expected 1 item after URL normalization dedup, got %d", len(result))
	}
	if result[0].Score != 80 {
		t.Errorf("expected higher-scored item kept, got score %.1f", result[0].Score)
	}
}

func TestDedupeItems_DifferentArticles(t *testing.T) {
	items := []IntelItem{
		{ID: "aaa", Title: "Go 1.23 released with new features", SourceURL: "https://go.dev/blog/1.23", Score: 80},
		{ID: "bbb", Title: "Rust 1.80 released with new features", SourceURL: "https://blog.rust-lang.org/1.80", Score: 75},
	}

	result := DedupeItems(items)
	if len(result) != 2 {
		t.Fatalf("expected 2 distinct items, got %d", len(result))
	}
}

func TestDedupeItems_TiebreakByEngagement(t *testing.T) {
	items := []IntelItem{
		{ID: "abc", Title: "Same article", Score: 80, Engagement: 100},
		{ID: "abc", Title: "Same article", Score: 80, Engagement: 500},
	}

	result := DedupeItems(items)
	if len(result) != 1 {
		t.Fatalf("expected 1 item, got %d", len(result))
	}
	if result[0].Engagement != 500 {
		t.Errorf("expected higher engagement item kept, got %d", result[0].Engagement)
	}
}

func TestDedupeItems_EmptyInput(t *testing.T) {
	result := DedupeItems(nil)
	if len(result) != 0 {
		t.Errorf("expected 0 items for nil input, got %d", len(result))
	}
}

func TestDedupeItems_SingleItem(t *testing.T) {
	items := []IntelItem{{ID: "a", Title: "Only one"}}
	result := DedupeItems(items)
	if len(result) != 1 {
		t.Errorf("expected 1 item, got %d", len(result))
	}
}

func TestDedupeItems_PreservesOrder(t *testing.T) {
	now := time.Now()
	items := []IntelItem{
		{ID: "a", Title: "First completely unique article about cooking", Score: 90, Timestamp: now},
		{ID: "b", Title: "Second totally different article on gardening", Score: 80, Timestamp: now},
		{ID: "c", Title: "Third unrelated piece about woodworking basics", Score: 70, Timestamp: now},
	}

	result := DedupeItems(items)
	if len(result) != 3 {
		t.Fatalf("expected 3 items, got %d", len(result))
	}
	if result[0].ID != "a" || result[1].ID != "b" || result[2].ID != "c" {
		t.Error("expected original order preserved for non-duplicates")
	}
}

func TestJaccardSimilarity(t *testing.T) {
	tests := []struct {
		a, b   string
		minSim float64
		maxSim float64
	}{
		// Identical → 1.0
		{"OpenAI releases GPT-5", "OpenAI releases GPT-5", 1.0, 1.0},
		// Very similar
		{"OpenAI releases GPT-5 with features", "OpenAI releases GPT-5 with capabilities", 0.6, 1.0},
		// Completely different
		{"Go programming language", "Python web framework", 0.0, 0.1},
		// Empty strings
		{"", "", 1.0, 1.0},
		{"something", "", 0.0, 0.0},
	}

	for _, tc := range tests {
		sim := jaccardSimilarity(tc.a, tc.b)
		if sim < tc.minSim || sim > tc.maxSim {
			t.Errorf("jaccardSimilarity(%q, %q) = %.2f, want [%.2f, %.2f]",
				tc.a, tc.b, sim, tc.minSim, tc.maxSim)
		}
	}
}

func TestCapItems_UnderLimit(t *testing.T) {
	items := []IntelItem{
		{ID: "a", Score: 80},
		{ID: "b", Score: 70},
	}
	result := CapItems(items, 50)
	if len(result) != 2 {
		t.Fatalf("expected 2 items when under cap, got %d", len(result))
	}
}

func TestCapItems_AtLimit(t *testing.T) {
	items := []IntelItem{
		{ID: "a", Score: 80},
		{ID: "b", Score: 70},
		{ID: "c", Score: 60},
	}
	result := CapItems(items, 3)
	if len(result) != 3 {
		t.Fatalf("expected 3 items at cap, got %d", len(result))
	}
}

func TestCapItems_OverLimit(t *testing.T) {
	items := []IntelItem{
		{ID: "a", Score: 50},
		{ID: "b", Score: 90},
		{ID: "c", Score: 70},
		{ID: "d", Score: 80},
		{ID: "e", Score: 60},
	}
	result := CapItems(items, 3)
	if len(result) != 3 {
		t.Fatalf("expected 3 items after cap, got %d", len(result))
	}
	// Should keep top 3 by score: 90, 80, 70
	if result[0].Score != 90 || result[1].Score != 80 || result[2].Score != 70 {
		t.Errorf("expected top 3 by score, got %.0f, %.0f, %.0f",
			result[0].Score, result[1].Score, result[2].Score)
	}
}

func TestCapItems_TiebreakByEngagement(t *testing.T) {
	items := []IntelItem{
		{ID: "a", Score: 80, Engagement: 100},
		{ID: "b", Score: 80, Engagement: 500},
		{ID: "c", Score: 80, Engagement: 200},
	}
	result := CapItems(items, 2)
	if len(result) != 2 {
		t.Fatalf("expected 2 items, got %d", len(result))
	}
	if result[0].Engagement != 500 || result[1].Engagement != 200 {
		t.Errorf("expected tiebreak by engagement (500, 200), got %d, %d",
			result[0].Engagement, result[1].Engagement)
	}
}

func TestDedupeByURL_CrossSource(t *testing.T) {
	seenURLs := make(map[string]IntelItem)

	// First source
	source1 := []IntelItem{
		{ID: "a", Title: "Article A", SourceURL: "https://example.com/post", Score: 70},
		{ID: "b", Title: "Article B", SourceURL: "https://other.com/page", Score: 60},
	}
	result1 := DedupeByURL(source1, seenURLs)
	if len(result1) != 2 {
		t.Fatalf("source1: expected 2 items, got %d", len(result1))
	}

	// Second source with overlapping URL
	source2 := []IntelItem{
		{ID: "c", Title: "Same article different source", SourceURL: "https://www.example.com/post?utm_source=twitter", Score: 90},
		{ID: "d", Title: "Unique article", SourceURL: "https://unique.com/article", Score: 50},
	}
	result2 := DedupeByURL(source2, seenURLs)
	if len(result2) != 1 {
		t.Fatalf("source2: expected 1 item (dup removed), got %d", len(result2))
	}
	if result2[0].ID != "d" {
		t.Errorf("expected unique article kept, got ID %s", result2[0].ID)
	}

	// The higher-scored duplicate should have replaced in source1's result
	if seenURLs[normalizeURL("https://example.com/post")].Score != 90 {
		t.Error("expected higher-scored duplicate to be tracked in seenURLs")
	}
}

func TestDedupeByURL_EmptyURL(t *testing.T) {
	seenURLs := make(map[string]IntelItem)
	items := []IntelItem{
		{ID: "a", Title: "No URL item", SourceURL: "", Score: 80},
		{ID: "b", Title: "Another no URL", SourceURL: "", Score: 70},
	}
	result := DedupeByURL(items, seenURLs)
	if len(result) != 2 {
		t.Fatalf("expected 2 items (empty URLs not deduped), got %d", len(result))
	}
}

func TestUniqueWords_FiltersShortWords(t *testing.T) {
	words := uniqueWords("I am a Go dev who likes AI")
	// "I", "am", "a", "Go", "AI" are ≤ 2 chars → filtered
	if words["go"] || words["ai"] || words["am"] {
		t.Error("expected short words (≤2 chars) to be filtered")
	}
	if !words["dev"] || !words["who"] || !words["likes"] {
		t.Error("expected words > 2 chars to be included")
	}
}
