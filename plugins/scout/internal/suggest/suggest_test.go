package suggest

import (
	"testing"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/gather"
)

// --- Helpers ---

// fixedTime returns a stable reference time for deterministic tests.
func fixedTime() time.Time {
	return time.Date(2026, 2, 18, 12, 0, 0, 0, time.UTC)
}

// makeItem builds an IntelItem with sensible defaults. Only non-zero fields need overriding.
func makeItem(title string, score float64, hoursAgo int, source string) gather.IntelItem {
	return gather.IntelItem{
		ID:        title, // unique enough for tests
		Title:     title,
		SourceURL: "https://" + source + ".com/" + title,
		Author:    "author",
		Score:     score,
		Timestamp: fixedTime().Add(-time.Duration(hoursAgo) * time.Hour),
	}
}

// makeFile builds an IntelFile for a given topic and source.
func makeFile(topic, source string, items ...gather.IntelItem) gather.IntelFile {
	return gather.IntelFile{
		Topic:      topic,
		Source:     source,
		GatheredAt: fixedTime(),
		Items:      items,
	}
}

// --- Analyze: empty and minimal inputs ---

func TestAnalyze_NilFiles(t *testing.T) {
	got := Analyze(nil, fixedTime(), 10)
	if got != nil {
		t.Fatalf("expected nil for nil files, got %d suggestions", len(got))
	}
}

func TestAnalyze_EmptyFiles(t *testing.T) {
	got := Analyze([]gather.IntelFile{}, fixedTime(), 10)
	if got != nil {
		t.Fatalf("expected nil for empty files, got %d suggestions", len(got))
	}
}

func TestAnalyze_NoKeywordsExtracted(t *testing.T) {
	// Titles with only short/stop words produce no keywords → no clusters
	files := []gather.IntelFile{
		makeFile("ai", "web",
			makeItem("the and for are", 80, 1, "web"),
			makeItem("but not you all", 70, 2, "web"),
		),
	}
	got := Analyze(files, fixedTime(), 10)
	if got != nil {
		t.Fatalf("expected nil when no keywords extracted, got %d", len(got))
	}
}

func TestAnalyze_SingleItem_NoClusters(t *testing.T) {
	// A single item cannot form a cluster (need 2+ items sharing keywords)
	files := []gather.IntelFile{
		makeFile("ai", "web",
			makeItem("Golang performance optimization guide", 80, 1, "web"),
		),
	}
	got := Analyze(files, fixedTime(), 10)
	if got != nil {
		t.Fatalf("expected nil for single item, got %d", len(got))
	}
}

// --- Clustering ---

func TestAnalyze_TwoItemsShareKeywords_FormCluster(t *testing.T) {
	// Two items sharing "golang" and "performance" should cluster
	files := []gather.IntelFile{
		makeFile("go", "hackernews",
			makeItem("Golang performance benchmarks released today", 80, 1, "hn"),
			makeItem("Golang performance optimization techniques explained", 75, 2, "hn"),
		),
	}
	got := Analyze(files, fixedTime(), 10)
	if len(got) == 0 {
		t.Fatal("expected at least 1 suggestion from keyword-pair cluster")
	}
	if got[0].Score <= 0 || got[0].Score > 100 {
		t.Errorf("score out of range: %d", got[0].Score)
	}
}

func TestAnalyze_NoSharedKeywords_NoClusters(t *testing.T) {
	// Items with completely different keywords should not cluster via pairs
	// (and each keyword appears in only 1 item, so no single-keyword fallback either)
	files := []gather.IntelFile{
		makeFile("mixed", "web",
			makeItem("Quantum computing breakthrough achieved", 80, 1, "web"),
			makeItem("Ancient Roman artifacts discovered underwater", 75, 2, "web"),
		),
	}
	got := Analyze(files, fixedTime(), 10)
	if len(got) != 0 {
		t.Fatalf("expected 0 suggestions for unrelated items, got %d", len(got))
	}
}

func TestAnalyze_SingleKeywordFallback(t *testing.T) {
	// 3+ items sharing a single keyword (but not a pair) trigger fallback clustering
	files := []gather.IntelFile{
		makeFile("tech", "web",
			makeItem("Kubernetes deployment strategies overview", 80, 1, "web"),
			makeItem("Kubernetes security scanning tools released", 70, 2, "web"),
			makeItem("Kubernetes networking deep explained", 60, 3, "web"),
		),
	}
	got := Analyze(files, fixedTime(), 10)
	if len(got) == 0 {
		t.Fatal("expected fallback single-keyword cluster for 3+ items sharing 'kubernetes'")
	}
}

func TestAnalyze_MultipleClusters_SortedByScore(t *testing.T) {
	// Two distinct clusters; higher-scored cluster should rank first
	files := []gather.IntelFile{
		makeFile("tech", "hackernews",
			// Cluster A: high scores, recent
			makeItem("Kubernetes security patches released urgently", 95, 1, "hn"),
			makeItem("Kubernetes security vulnerabilities discovered widespread", 90, 1, "hn"),
		),
		makeFile("tech", "reddit",
			// Cluster B: lower scores, older
			makeItem("Python framework comparison testing results", 40, 48, "reddit"),
			makeItem("Python framework benchmarks testing complete", 35, 72, "reddit"),
		),
	}
	got := Analyze(files, fixedTime(), 10)
	if len(got) < 2 {
		t.Fatalf("expected 2 clusters, got %d", len(got))
	}
	if got[0].Score < got[1].Score {
		t.Errorf("suggestions not sorted descending: %d < %d", got[0].Score, got[1].Score)
	}
}

// --- Limit ---

func TestAnalyze_LimitTruncates(t *testing.T) {
	// Build enough items for multiple clusters, then limit to 1
	files := []gather.IntelFile{
		makeFile("ai", "web",
			makeItem("Kubernetes security patches released today", 90, 1, "web"),
			makeItem("Kubernetes security vulnerabilities found critical", 85, 2, "web"),
		),
		makeFile("ai", "hn",
			makeItem("Python framework comparison benchmark results", 80, 1, "hn"),
			makeItem("Python framework performance benchmark analysis", 75, 2, "hn"),
		),
	}
	got := Analyze(files, fixedTime(), 1)
	if len(got) != 1 {
		t.Fatalf("expected 1 suggestion with limit=1, got %d", len(got))
	}
}

func TestAnalyze_LimitZero_NoTruncation(t *testing.T) {
	files := []gather.IntelFile{
		makeFile("ai", "web",
			makeItem("Kubernetes security patches released today", 90, 1, "web"),
			makeItem("Kubernetes security vulnerabilities found critical", 85, 2, "web"),
		),
	}
	got := Analyze(files, fixedTime(), 0)
	// limit <= 0 means no truncation
	if len(got) == 0 {
		t.Fatal("expected suggestions with limit=0 (no truncation)")
	}
}

// --- Scoring signals ---

func TestScoring_RecentItemsScoreHigher(t *testing.T) {
	// Recent cluster vs old cluster — same engagement, same sources
	recent := []gather.IntelFile{
		makeFile("ai", "web",
			makeItem("Machine learning deployment strategies overview", 80, 1, "web"),
			makeItem("Machine learning deployment pipeline explained", 75, 2, "web"),
		),
	}
	old := []gather.IntelFile{
		makeFile("ai", "web",
			makeItem("Machine learning deployment strategies overview", 80, 160, "web"),
			makeItem("Machine learning deployment pipeline explained", 75, 165, "web"),
		),
	}

	recentResult := Analyze(recent, fixedTime(), 1)
	oldResult := Analyze(old, fixedTime(), 1)

	if len(recentResult) == 0 || len(oldResult) == 0 {
		t.Fatal("expected suggestions from both recent and old data")
	}
	if recentResult[0].Score <= oldResult[0].Score {
		t.Errorf("recent items should score higher: recent=%d, old=%d",
			recentResult[0].Score, oldResult[0].Score)
	}
}

func TestScoring_HighEngagementScoresHigher(t *testing.T) {
	highEng := []gather.IntelFile{
		makeFile("ai", "web",
			makeItem("Artificial intelligence research breakthrough announced", 95, 1, "web"),
			makeItem("Artificial intelligence research paper breakthrough", 90, 2, "web"),
		),
	}
	lowEng := []gather.IntelFile{
		makeFile("ai", "web",
			makeItem("Artificial intelligence research breakthrough announced", 10, 1, "web"),
			makeItem("Artificial intelligence research paper breakthrough", 5, 2, "web"),
		),
	}

	highResult := Analyze(highEng, fixedTime(), 1)
	lowResult := Analyze(lowEng, fixedTime(), 1)

	if len(highResult) == 0 || len(lowResult) == 0 {
		t.Fatal("expected suggestions from both high and low engagement data")
	}
	if highResult[0].Score <= lowResult[0].Score {
		t.Errorf("high-engagement items should score higher: high=%d, low=%d",
			highResult[0].Score, lowResult[0].Score)
	}
}

func TestScoring_MultipleSources_BoostsTrend(t *testing.T) {
	// Same items but from multiple sources (higher trend signal)
	multiSource := []gather.IntelFile{
		makeFile("ai", "hackernews",
			makeItem("Golang compiler improvements released version", 80, 1, "hn"),
			makeItem("Golang compiler optimization released update", 75, 2, "hn"),
		),
		makeFile("ai", "reddit",
			makeItem("Golang compiler improvements released announcement", 80, 1, "reddit"),
		),
		makeFile("ai", "devto",
			makeItem("Golang compiler optimization released announcement", 75, 1, "devto"),
		),
	}
	singleSource := []gather.IntelFile{
		makeFile("ai", "web",
			makeItem("Golang compiler improvements released version", 80, 1, "web"),
			makeItem("Golang compiler optimization released update", 75, 2, "web"),
		),
	}

	multiResult := Analyze(multiSource, fixedTime(), 1)
	singleResult := Analyze(singleSource, fixedTime(), 1)

	if len(multiResult) == 0 || len(singleResult) == 0 {
		t.Fatal("expected suggestions from both datasets")
	}
	if multiResult[0].Score <= singleResult[0].Score {
		t.Errorf("multi-source should score higher via trend: multi=%d, single=%d",
			multiResult[0].Score, singleResult[0].Score)
	}
}

// --- Cross-topic detection ---

func TestScoring_CrossTopic_BoostsScore(t *testing.T) {
	crossTopic := []gather.IntelFile{
		makeFile("ai-models", "web",
			makeItem("Golang performance optimization benchmark results", 80, 1, "web"),
		),
		makeFile("go-development", "web",
			makeItem("Golang performance testing benchmark analysis", 75, 2, "web"),
		),
		makeFile("developer-tools", "web",
			makeItem("Golang performance profiling benchmark tools", 70, 3, "web"),
		),
	}
	singleTopic := []gather.IntelFile{
		makeFile("go-development", "web",
			makeItem("Golang performance optimization benchmark results", 80, 1, "web"),
			makeItem("Golang performance testing benchmark analysis", 75, 2, "web"),
			makeItem("Golang performance profiling benchmark tools", 70, 3, "web"),
		),
	}

	crossResult := Analyze(crossTopic, fixedTime(), 1)
	singleResult := Analyze(singleTopic, fixedTime(), 1)

	if len(crossResult) == 0 || len(singleResult) == 0 {
		t.Fatal("expected suggestions from both cross-topic and single-topic")
	}
	if crossResult[0].Score <= singleResult[0].Score {
		t.Errorf("cross-topic should score higher: cross=%d, single=%d",
			crossResult[0].Score, singleResult[0].Score)
	}
}

func TestCrossTopic_TopicsFieldPopulated(t *testing.T) {
	files := []gather.IntelFile{
		makeFile("ai-models", "web",
			makeItem("Golang performance optimization benchmark results", 80, 1, "web"),
		),
		makeFile("go-development", "web",
			makeItem("Golang performance testing benchmark analysis", 75, 2, "web"),
		),
	}
	got := Analyze(files, fixedTime(), 10)
	if len(got) == 0 {
		t.Fatal("expected suggestions")
	}
	if len(got[0].Topics) < 2 {
		t.Errorf("expected 2+ topics for cross-topic cluster, got %v", got[0].Topics)
	}
}

// --- Score bounds ---

func TestScoring_ScoreBounds(t *testing.T) {
	// Construct maximum-signal cluster: many sources, high scores, very recent, cross-topic
	var files []gather.IntelFile
	sources := []string{"hackernews", "reddit", "devto", "lobsters", "web"}
	topics := []string{"ai-models", "go-development", "developer-tools"}
	for i, src := range sources {
		topic := topics[i%len(topics)]
		files = append(files, makeFile(topic, src,
			makeItem("Kubernetes orchestration platform released update", 100, 0, src),
			makeItem("Kubernetes orchestration container released version", 95, 0, src),
		))
	}

	got := Analyze(files, fixedTime(), 1)
	if len(got) == 0 {
		t.Fatal("expected suggestion")
	}
	if got[0].Score < 0 || got[0].Score > 100 {
		t.Errorf("score out of [0,100]: %d", got[0].Score)
	}
}

func TestScoring_OldLowEngagement_LowScore(t *testing.T) {
	files := []gather.IntelFile{
		makeFile("misc", "web",
			makeItem("Obscure programming language released today", 5, 167, "web"),
			makeItem("Obscure programming framework released update", 3, 168, "web"),
		),
	}
	got := Analyze(files, fixedTime(), 1)
	if len(got) == 0 {
		t.Fatal("expected suggestion")
	}
	// Old + low engagement + single source + single topic → low score
	if got[0].Score > 30 {
		t.Errorf("expected low score for old/low-engagement items, got %d", got[0].Score)
	}
}

// --- Content type inference ---

func TestInferContentType_Thread(t *testing.T) {
	// trend >= 0.6 and itemCount >= 3
	got := inferContentType(0.7, 0.3, 0.2, 4)
	if got != "thread" {
		t.Errorf("expected 'thread', got %q", got)
	}
}

func TestInferContentType_Video(t *testing.T) {
	// crossTopic >= 0.5 and engagement >= 0.4 (and not meeting thread criteria)
	got := inferContentType(0.3, 0.5, 0.6, 2)
	if got != "video" {
		t.Errorf("expected 'video', got %q", got)
	}
}

func TestInferContentType_Blog_Default(t *testing.T) {
	got := inferContentType(0.2, 0.2, 0.2, 2)
	if got != "blog" {
		t.Errorf("expected 'blog', got %q", got)
	}
}

func TestInferContentType_ThreadTakesPrecedence(t *testing.T) {
	// Meets both thread AND video criteria — thread should win
	got := inferContentType(0.8, 0.9, 0.9, 5)
	if got != "thread" {
		t.Errorf("expected 'thread' to take precedence, got %q", got)
	}
}

// --- extractKeywords ---

func TestExtractKeywords_Basic(t *testing.T) {
	kw := extractKeywords("Golang Performance Optimization Guide")
	// "golang", "performance", "optimization", "guide" all > 3 chars, not stop words
	for _, want := range []string{"golang", "performance", "optimization", "guide"} {
		if !kw[want] {
			t.Errorf("expected keyword %q", want)
		}
	}
}

func TestExtractKeywords_FiltersShortAndStopWords(t *testing.T) {
	kw := extractKeywords("the new AI for all of us")
	// "the" = stop word, "new" = stop word, "for" = stop word, "all" = stop word
	// "ai" = 2 chars ≤ 3, "of" = 2 chars, "us" = 2 chars
	if len(kw) != 0 {
		t.Errorf("expected 0 keywords from stop/short words, got %v", kw)
	}
}

func TestExtractKeywords_StripsPunctuation(t *testing.T) {
	kw := extractKeywords("(Golang) performance, benchmarks!")
	if !kw["golang"] || !kw["performance"] || !kw["benchmarks"] {
		t.Errorf("expected punctuation stripped, got %v", kw)
	}
}

func TestExtractKeywords_EmptyTitle(t *testing.T) {
	kw := extractKeywords("")
	if len(kw) != 0 {
		t.Errorf("expected 0 keywords for empty title, got %d", len(kw))
	}
}

// --- buildTitle ---

func TestBuildTitle_ShortTitle_UsesDirectly(t *testing.T) {
	items := []*annotatedItem{
		{title: "Short Title Here", score: 90},
		{title: "Another Item", score: 50},
	}
	got := buildTitle([]string{"short", "title"}, items)
	if got != "Short Title Here" {
		t.Errorf("expected best item's short title, got %q", got)
	}
}

func TestBuildTitle_LongTitle_UsesLabelPrefix(t *testing.T) {
	longTitle := "This is a very long title that exceeds eighty characters in length and should be truncated by the build title function"
	items := []*annotatedItem{
		{title: longTitle, score: 90},
		{title: "Short one", score: 50},
	}
	got := buildTitle([]string{"long", "title"}, items)
	if len(got) == 0 {
		t.Fatal("expected non-empty title")
	}
	// Should start with capitalized label
	if got[:4] != "Long" {
		t.Errorf("expected label prefix 'Long ...', got %q", got[:20])
	}
}

// --- buildAngle ---

func TestBuildAngle_CrossTopic(t *testing.T) {
	items := []*annotatedItem{
		{topic: "ai", source: "web"},
		{topic: "go", source: "hn"},
	}
	got := buildAngle(items, []string{"test"})
	if got == "" {
		t.Fatal("expected non-empty angle")
	}
	// Cross-topic angle starts with "Cross-topic analysis"
	if len(got) < 14 || got[:14] != "Cross-topic an" {
		t.Errorf("expected cross-topic angle, got %q", got)
	}
}

func TestBuildAngle_MultipleSources(t *testing.T) {
	items := []*annotatedItem{
		{topic: "ai", source: "web"},
		{topic: "ai", source: "hn"},
		{topic: "ai", source: "reddit"},
	}
	got := buildAngle(items, []string{"test"})
	if got == "" || got[:7] != "Roundup" {
		t.Errorf("expected 'Roundup' angle for 3+ sources same topic, got %q", got)
	}
}

func TestBuildAngle_DeepDive(t *testing.T) {
	items := []*annotatedItem{
		{topic: "ai", source: "web"},
		{topic: "ai", source: "hn"},
		{topic: "ai", source: "web"}, // same source — only 2 distinct sources
		{topic: "ai", source: "hn"},
	}
	got := buildAngle(items, []string{"test"})
	if got == "" || got[:9] != "Deep dive" {
		t.Errorf("expected 'Deep dive' angle for 4+ items / 2 sources, got %q", got)
	}
}

func TestBuildAngle_CompareDefault(t *testing.T) {
	items := []*annotatedItem{
		{topic: "ai", source: "web"},
		{topic: "ai", source: "web"},
	}
	got := buildAngle(items, []string{"test"})
	if got == "" || got[:7] != "Compare" {
		t.Errorf("expected 'Compare' default angle, got %q", got)
	}
}

func TestBuildAngle_SingleItem_Empty(t *testing.T) {
	items := []*annotatedItem{{topic: "ai", source: "web"}}
	got := buildAngle(items, []string{"test"})
	if got != "" {
		t.Errorf("expected empty angle for single item, got %q", got)
	}
}

// --- buildSources ---

func TestBuildSources_SortsByScore(t *testing.T) {
	items := []*annotatedItem{
		{title: "Low", sourceURL: "https://a.com/1", score: 20},
		{title: "High", sourceURL: "https://b.com/2", score: 90},
		{title: "Mid", sourceURL: "https://c.com/3", score: 50},
	}
	got := buildSources(items)
	if len(got) != 3 {
		t.Fatalf("expected 3 sources, got %d", len(got))
	}
	if got[0].Score != 90 || got[1].Score != 50 || got[2].Score != 20 {
		t.Errorf("sources not sorted by score: %d, %d, %d", got[0].Score, got[1].Score, got[2].Score)
	}
}

func TestBuildSources_CapsAtFive(t *testing.T) {
	var items []*annotatedItem
	for i := 0; i < 8; i++ {
		items = append(items, &annotatedItem{
			title:     "Item",
			sourceURL: "https://example.com/" + itoa(i),
			score:     float64(i * 10),
		})
	}
	got := buildSources(items)
	if len(got) != 5 {
		t.Errorf("expected max 5 sources, got %d", len(got))
	}
}

// --- domainFromURL ---

func TestDomainFromURL(t *testing.T) {
	tests := []struct {
		url  string
		want string
	}{
		{"https://news.ycombinator.com/item?id=123", "news.ycombinator.com"},
		{"http://example.com/post", "example.com"},
		{"https://dev.to/user/post", "dev.to"},
		{"", "unknown"},
		{"ftp://files.example.com/readme", "files.example.com"},
	}
	for _, tc := range tests {
		got := domainFromURL(tc.url)
		if got != tc.want {
			t.Errorf("domainFromURL(%q) = %q, want %q", tc.url, got, tc.want)
		}
	}
}

// --- Helper functions ---

func TestTruncate(t *testing.T) {
	if got := truncate("short", 10); got != "short" {
		t.Errorf("expected no truncation, got %q", got)
	}
	if got := truncate("a long string here", 6); got != "a long..." {
		t.Errorf("expected truncation, got %q", got)
	}
}

func TestPluralize(t *testing.T) {
	if got := pluralize(1, "source"); got != "1 source" {
		t.Errorf("expected singular, got %q", got)
	}
	if got := pluralize(3, "source"); got != "3 sources" {
		t.Errorf("expected plural, got %q", got)
	}
}

func TestCapitalize(t *testing.T) {
	tests := []struct{ in, want string }{
		{"golang", "Golang"},
		{"", ""},
		{"Already", "Already"},
		{"123num", "123num"},
	}
	for _, tc := range tests {
		if got := capitalize(tc.in); got != tc.want {
			t.Errorf("capitalize(%q) = %q, want %q", tc.in, got, tc.want)
		}
	}
}

func TestItoa(t *testing.T) {
	tests := []struct {
		n    int
		want string
	}{
		{0, "0"},
		{42, "42"},
		{-7, "-7"},
		{100, "100"},
	}
	for _, tc := range tests {
		if got := itoa(tc.n); got != tc.want {
			t.Errorf("itoa(%d) = %q, want %q", tc.n, got, tc.want)
		}
	}
}

func TestIsStopWord(t *testing.T) {
	stops := []string{"the", "and", "with", "from", "this", "that", "using"}
	for _, w := range stops {
		if !isStopWord(w) {
			t.Errorf("expected %q to be a stop word", w)
		}
	}
	nonStops := []string{"golang", "performance", "kubernetes", "benchmark"}
	for _, w := range nonStops {
		if isStopWord(w) {
			t.Errorf("expected %q NOT to be a stop word", w)
		}
	}
}

func TestJoinKeys(t *testing.T) {
	m := map[string]bool{"b": true, "a": true, "c": true}
	got := joinKeys(m)
	if got != "a, b, c" {
		t.Errorf("expected sorted keys, got %q", got)
	}
}

// --- Intersect ---

func TestIntersect(t *testing.T) {
	got := intersect([]int{1, 2, 3, 4}, []int{3, 4, 5, 6})
	if len(got) != 2 {
		t.Fatalf("expected 2 common elements, got %d", len(got))
	}
}

func TestIntersect_NoOverlap(t *testing.T) {
	got := intersect([]int{1, 2}, []int{3, 4})
	if len(got) != 0 {
		t.Errorf("expected 0 common elements, got %d", len(got))
	}
}

func TestIntersect_Empty(t *testing.T) {
	got := intersect(nil, []int{1, 2})
	if len(got) != 0 {
		t.Errorf("expected 0 for nil input, got %d", len(got))
	}
}

// --- Suggestion struct fields ---

func TestSuggestion_HasRequiredFields(t *testing.T) {
	files := []gather.IntelFile{
		makeFile("ai", "web",
			makeItem("Golang performance optimization benchmark results", 80, 1, "web"),
			makeItem("Golang performance testing benchmark analysis", 75, 2, "web"),
		),
	}
	got := Analyze(files, fixedTime(), 10)
	if len(got) == 0 {
		t.Fatal("expected suggestion")
	}
	s := got[0]
	if s.Title == "" {
		t.Error("Title should not be empty")
	}
	if s.ContentType != "blog" && s.ContentType != "thread" && s.ContentType != "video" {
		t.Errorf("unexpected ContentType: %q", s.ContentType)
	}
	if len(s.Topics) == 0 {
		t.Error("Topics should not be empty")
	}
	if len(s.Sources) == 0 {
		t.Error("Sources should not be empty")
	}
}

// --- Time filtering (recency decay) ---

func TestRecencyDecay_ZeroAge_FullScore(t *testing.T) {
	// Items at time=now should get recencyScore = 1.0
	files := []gather.IntelFile{
		makeFile("ai", "web",
			makeItem("Kubernetes container orchestration platform released", 50, 0, "web"),
			makeItem("Kubernetes container management platform update", 50, 0, "web"),
		),
	}
	got := Analyze(files, fixedTime(), 1)
	if len(got) == 0 {
		t.Fatal("expected suggestion")
	}
	// With maxed recency (25pts), moderate engagement, 1 source, 1 topic
	// Score should be reasonable
	if got[0].Score < 20 {
		t.Errorf("very recent items should have decent score, got %d", got[0].Score)
	}
}

func TestRecencyDecay_BeyondWeek_ZeroRecency(t *testing.T) {
	// Items older than 168h should get recencyScore = 0
	files := []gather.IntelFile{
		makeFile("ai", "web",
			makeItem("Kubernetes container orchestration released update", 50, 200, "web"),
			makeItem("Kubernetes container management released version", 50, 200, "web"),
		),
	}
	got := Analyze(files, fixedTime(), 1)
	if len(got) == 0 {
		t.Fatal("expected suggestion")
	}
	// Same items but much older — should score lower
	freshFiles := []gather.IntelFile{
		makeFile("ai", "web",
			makeItem("Kubernetes container orchestration released update", 50, 0, "web"),
			makeItem("Kubernetes container management released version", 50, 0, "web"),
		),
	}
	fresh := Analyze(freshFiles, fixedTime(), 1)
	if len(fresh) == 0 {
		t.Fatal("expected suggestion")
	}
	if got[0].Score >= fresh[0].Score {
		t.Errorf("old items (%d) should score lower than fresh (%d)", got[0].Score, fresh[0].Score)
	}
}
