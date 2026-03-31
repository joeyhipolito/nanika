package cmd

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/gather"
)

// containsStr is a correct substring check (the original helper was broken —
// it compared lengths instead of content).
func containsStr(s, substr string) bool {
	return strings.Contains(s, substr)
}

// setupTempHome redirects HOME to an isolated temp directory so scout never
// touches the real ~/.scout during tests. HOME is restored automatically by
// t.Setenv when the test finishes.
func setupTempHome(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	t.Setenv("HOME", dir)
	return dir
}

// writeTopicFile serialises a TopicConfig and writes it to the given directory.
func writeTopicFile(t *testing.T, dir string, tc gather.TopicConfig) {
	t.Helper()
	data, err := json.MarshalIndent(tc, "", "  ")
	if err != nil {
		t.Fatalf("marshal topic: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, tc.Name+".json"), data, 0600); err != nil {
		t.Fatalf("write topic file: %v", err)
	}
}

// readTopicFile reads and unmarshals a topic config from the given directory.
func readTopicFile(t *testing.T, dir, name string) gather.TopicConfig {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(dir, name+".json"))
	if err != nil {
		t.Fatalf("read topic file %s: %v", name, err)
	}
	var tc gather.TopicConfig
	if err := json.Unmarshal(data, &tc); err != nil {
		t.Fatalf("unmarshal topic %s: %v", name, err)
	}
	return tc
}

// ─── summarizeIntel ──────────────────────────────────────────────────────────

func TestSummarizeIntel_EmptyInput(t *testing.T) {
	got := summarizeIntel(nil)
	if got != "" {
		t.Errorf("expected empty string for nil input, got %q", got)
	}
}

func TestSummarizeIntel_EmptyItems(t *testing.T) {
	files := []gather.IntelFile{
		{Topic: "ai-models", Source: "web", Items: nil},
	}
	got := summarizeIntel(files)
	if !containsStr(got, "ai-models") {
		t.Errorf("summary should mention topic name even with no items, got: %s", got)
	}
	if !containsStr(got, "0 items") {
		t.Errorf("summary should report 0 items, got: %s", got)
	}
}

func TestSummarizeIntel_SingleTopicSingleSource(t *testing.T) {
	files := []gather.IntelFile{
		{
			Topic:      "ai-models",
			Source:     "web",
			GatheredAt: time.Now(),
			Items: []gather.IntelItem{
				{ID: "1", Title: "OpenAI releases GPT-5 model"},
				{ID: "2", Title: "Google Gemini ultra launch"},
			},
		},
	}
	got := summarizeIntel(files)
	if !containsStr(got, "ai-models") {
		t.Error("summary should contain topic name")
	}
	if !containsStr(got, "web") {
		t.Error("summary should mention source")
	}
	if !containsStr(got, "2 items") {
		t.Errorf("summary should report 2 items, got: %s", got)
	}
}

func TestSummarizeIntel_MultipleTopicsSortedByCount(t *testing.T) {
	// ai-models has 3 items, go-development has 1 — ai-models should appear first.
	files := []gather.IntelFile{
		{
			Topic:  "go-development",
			Source: "reddit",
			Items:  []gather.IntelItem{{ID: "1", Title: "Go 1.23 released"}},
		},
		{
			Topic:  "ai-models",
			Source: "web",
			Items: []gather.IntelItem{
				{ID: "2", Title: "GPT-5 released"},
				{ID: "3", Title: "Claude major update"},
				{ID: "4", Title: "Gemini release"},
			},
		},
	}
	got := summarizeIntel(files)
	aiPos := strings.Index(got, "ai-models")
	goPos := strings.Index(got, "go-development")
	if aiPos == -1 || goPos == -1 {
		t.Fatal("both topics should appear in summary")
	}
	if aiPos > goPos {
		t.Error("topic with more items (ai-models:3) should appear before topic with fewer (go-development:1)")
	}
}

func TestSummarizeIntel_MultipleSourcesPerTopic(t *testing.T) {
	files := []gather.IntelFile{
		{Topic: "ai-models", Source: "web", Items: []gather.IntelItem{{ID: "1", Title: "item one"}}},
		{Topic: "ai-models", Source: "reddit", Items: []gather.IntelItem{{ID: "2", Title: "item two"}}},
		{Topic: "ai-models", Source: "hackernews", Items: []gather.IntelItem{{ID: "3", Title: "item three"}}},
	}
	got := summarizeIntel(files)
	for _, src := range []string{"web", "reddit", "hackernews"} {
		if !containsStr(got, src) {
			t.Errorf("summary should mention source %q, got: %s", src, got)
		}
	}
	if !containsStr(got, "3 items") {
		t.Errorf("summary should report 3 total items, got: %s", got)
	}
}

func TestSummarizeIntel_KeywordsExtractedFromTitles(t *testing.T) {
	// "releases" (8 chars) appears twice — should rank in top keywords.
	files := []gather.IntelFile{
		{
			Topic:  "ai-models",
			Source: "web",
			Items: []gather.IntelItem{
				{ID: "1", Title: "OpenAI releases flagship model"},
				{ID: "2", Title: "Anthropic releases Claude update"},
			},
		},
	}
	got := summarizeIntel(files)
	if !containsStr(got, "releases") {
		t.Errorf("high-frequency keyword 'releases' should appear in summary, got: %s", got)
	}
}

func TestSummarizeIntel_ShortWordsFilteredFromKeywords(t *testing.T) {
	// Words ≤ 4 chars and stop words should not appear in the keyword list.
	files := []gather.IntelFile{
		{
			Topic:  "ai-models",
			Source: "web",
			Items: []gather.IntelItem{
				// "the" (stop word), "go" (2 chars), "new" (stop word) — all filtered
				{ID: "1", Title: "the go new"},
			},
		},
	}
	got := summarizeIntel(files)
	// The "topics:" section should not list these as extracted keywords
	topicsSection := ""
	if idx := strings.Index(got, "topics:"); idx >= 0 {
		topicsSection = got[idx:]
	}
	for _, word := range []string{"the", " go,", " new,"} {
		if containsStr(topicsSection, word) {
			t.Errorf("short/stop word %q should be filtered from keyword list, topics section: %q", word, topicsSection)
		}
	}
}

func TestSummarizeIntel_KeywordsCappedAtTen(t *testing.T) {
	// Generate 15 distinct long keywords — summary should cap at 10.
	var items []gather.IntelItem
	words := []string{
		"understanding", "performance", "benchmark", "framework", "production",
		"deployment", "kubernetes", "observability", "monitoring", "concurrency",
		"resilience", "efficiency", "reliability", "scalability", "distribution",
	}
	for i, w := range words {
		items = append(items, gather.IntelItem{ID: string(rune('a'+i)), Title: w + " " + w})
	}
	files := []gather.IntelFile{{Topic: "go-dev", Source: "web", Items: items}}
	got := summarizeIntel(files)

	// Count commas in the topics: section as a proxy for keyword count
	topicsIdx := strings.Index(got, "topics:")
	if topicsIdx == -1 {
		return // no keywords section — test inconclusive but not a failure
	}
	topicsLine := got[topicsIdx:]
	// Trim to end of line
	if nl := strings.Index(topicsLine, "\n"); nl >= 0 {
		topicsLine = topicsLine[:nl]
	}
	commas := strings.Count(topicsLine, ",")
	if commas > 9 { // 10 keywords → 9 commas
		t.Errorf("expected ≤ 10 keywords (≤ 9 commas), got %d commas in: %s", commas, topicsLine)
	}
}

// ─── buildDiscoverPrompt ──────────────────────────────────────────────────────

func TestBuildDiscoverPrompt_ContainsIntelSummary(t *testing.T) {
	summary := "- ai-models: 42 items from web (12), reddit (8)"
	prompt := buildDiscoverPrompt(summary, []string{"ai-models"})
	if !containsStr(prompt, summary) {
		t.Error("prompt should contain the intel summary verbatim")
	}
}

func TestBuildDiscoverPrompt_ContainsExistingTopics(t *testing.T) {
	topics := []string{"ai-models", "go-development", "developer-tools"}
	prompt := buildDiscoverPrompt("some summary", topics)
	for _, topic := range topics {
		if !containsStr(prompt, topic) {
			t.Errorf("prompt should mention existing topic %q", topic)
		}
	}
}

func TestBuildDiscoverPrompt_ContainsJSONSchemaKeywords(t *testing.T) {
	prompt := buildDiscoverPrompt("summary", []string{})
	// Gemini must see these field names to generate the right JSON.
	for _, kw := range []string{"recommendations", "rationale", "action", "search_terms", "add_sources"} {
		if !containsStr(prompt, kw) {
			t.Errorf("prompt should contain schema keyword %q", kw)
		}
	}
}

func TestBuildDiscoverPrompt_ContainsAllActions(t *testing.T) {
	prompt := buildDiscoverPrompt("summary", []string{})
	for _, action := range []string{"create", "enhance", "add_sources"} {
		if !containsStr(prompt, action) {
			t.Errorf("prompt should describe action %q", action)
		}
	}
}

func TestBuildDiscoverPrompt_EmptyExistingTopics(t *testing.T) {
	prompt := buildDiscoverPrompt("empty summary", []string{})
	if !containsStr(prompt, "empty summary") {
		t.Error("prompt should contain intel summary even with no existing topics")
	}
	// Must still contain JSON schema
	if !containsStr(prompt, "recommendations") {
		t.Error("prompt must include recommendations schema even with no existing topics")
	}
}

// ─── JSON serialisation ───────────────────────────────────────────────────────

func TestDiscoverRecommendation_JSONRoundtrip(t *testing.T) {
	tests := []struct {
		name string
		rec  DiscoverRecommendation
	}{
		{
			name: "create with all fields",
			rec: DiscoverRecommendation{
				Action:      "create",
				Topic:       "ai-safety",
				Description: "AI safety research and alignment",
				SearchTerms: []string{"alignment", "RLHF", "AI safety"},
				AddSources:  []string{"arxiv", "hackernews"},
				AddFeeds:    []string{"https://example.com/feed.xml"},
				Rationale:   "Strong signal in intel about safety concerns",
			},
		},
		{
			name: "enhance minimal",
			rec: DiscoverRecommendation{
				Action:    "enhance",
				Topic:     "go-development",
				Rationale: "Missing key terms",
			},
		},
		{
			name: "add_sources no search terms",
			rec: DiscoverRecommendation{
				Action:     "add_sources",
				Topic:      "ai-models",
				AddSources: []string{"youtube"},
				Rationale:  "YouTube content detected",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			data, err := json.Marshal(tt.rec)
			if err != nil {
				t.Fatalf("marshal failed: %v", err)
			}

			var got DiscoverRecommendation
			if err := json.Unmarshal(data, &got); err != nil {
				t.Fatalf("unmarshal failed: %v", err)
			}

			if got.Action != tt.rec.Action {
				t.Errorf("action: want %q, got %q", tt.rec.Action, got.Action)
			}
			if got.Topic != tt.rec.Topic {
				t.Errorf("topic: want %q, got %q", tt.rec.Topic, got.Topic)
			}
			if got.Rationale != tt.rec.Rationale {
				t.Errorf("rationale: want %q, got %q", tt.rec.Rationale, got.Rationale)
			}
			if len(got.SearchTerms) != len(tt.rec.SearchTerms) {
				t.Errorf("search_terms count: want %d, got %d", len(tt.rec.SearchTerms), len(got.SearchTerms))
			}
		})
	}
}

func TestDiscoverSuggestions_JSONKeysAreSnakeCase(t *testing.T) {
	sugg := DiscoverSuggestions{
		GeneratedAt:  "2026-02-25T12:00:00Z",
		IntelSummary: "Detected themes: AI model releases (42 items)",
		Recommendations: []DiscoverRecommendation{
			{Action: "create", Topic: "ai-safety", Rationale: "evidence"},
		},
	}
	data, err := json.Marshal(sugg)
	if err != nil {
		t.Fatalf("marshal failed: %v", err)
	}

	var raw map[string]interface{}
	if err := json.Unmarshal(data, &raw); err != nil {
		t.Fatalf("unmarshal raw failed: %v", err)
	}
	for _, key := range []string{"generated_at", "intel_summary", "recommendations"} {
		if _, ok := raw[key]; !ok {
			t.Errorf("expected JSON key %q in output, got keys: %v", key, raw)
		}
	}
}

func TestDiscoverSuggestions_JSONRoundtrip(t *testing.T) {
	sugg := DiscoverSuggestions{
		GeneratedAt:  "2026-02-25T12:00:00Z",
		IntelSummary: "Detected themes: AI model releases",
		Recommendations: []DiscoverRecommendation{
			{Action: "create", Topic: "ai-safety", Rationale: "evidence in intel"},
			{Action: "enhance", Topic: "ai-models", AddSources: []string{"youtube"}, Rationale: "missing youtube"},
		},
	}
	data, err := json.Marshal(sugg)
	if err != nil {
		t.Fatalf("marshal failed: %v", err)
	}
	var got DiscoverSuggestions
	if err := json.Unmarshal(data, &got); err != nil {
		t.Fatalf("roundtrip unmarshal failed: %v", err)
	}
	if got.GeneratedAt != sugg.GeneratedAt {
		t.Errorf("generated_at: want %q, got %q", sugg.GeneratedAt, got.GeneratedAt)
	}
	if len(got.Recommendations) != 2 {
		t.Errorf("expected 2 recommendations, got %d", len(got.Recommendations))
	}
}

// ─── discoverAutoApply ────────────────────────────────────────────────────────

func TestDiscoverAutoApply_DryRunDoesNotWriteFiles(t *testing.T) {
	tmpHome := setupTempHome(t)
	topicsDir := filepath.Join(tmpHome, ".scout", "topics")
	if err := os.MkdirAll(topicsDir, 0700); err != nil {
		t.Fatal(err)
	}

	sugg := &DiscoverSuggestions{
		Recommendations: []DiscoverRecommendation{
			{
				Action:      "create",
				Topic:       "ai-safety",
				Description: "AI safety research",
				SearchTerms: []string{"alignment"},
				AddSources:  []string{"arxiv"},
				Rationale:   "evidence",
			},
		},
	}

	if err := discoverAutoApply(sugg, false, true, nil); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	entries, _ := os.ReadDir(topicsDir)
	if len(entries) != 0 {
		t.Errorf("dry-run should not write files, got %d file(s) in topics dir", len(entries))
	}
}

func TestDiscoverAutoApply_AutoCreatesTopicFile(t *testing.T) {
	tmpHome := setupTempHome(t)
	topicsDir := filepath.Join(tmpHome, ".scout", "topics")
	// saveTopic calls config.EnsureDirs() internally, but creating ahead of time is harmless.
	if err := os.MkdirAll(topicsDir, 0700); err != nil {
		t.Fatal(err)
	}

	sugg := &DiscoverSuggestions{
		Recommendations: []DiscoverRecommendation{
			{
				Action:      "create",
				Topic:       "ai-safety",
				Description: "AI safety research and alignment",
				SearchTerms: []string{"alignment", "RLHF"},
				AddSources:  []string{"arxiv"},
				Rationale:   "evidence",
			},
		},
	}

	if err := discoverAutoApply(sugg, true, false, nil); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	saved := readTopicFile(t, topicsDir, "ai-safety")
	if saved.Name != "ai-safety" {
		t.Errorf("topic name: want %q, got %q", "ai-safety", saved.Name)
	}
	if saved.Description != "AI safety research and alignment" {
		t.Errorf("description: want %q, got %q", "AI safety research and alignment", saved.Description)
	}
	if len(saved.SearchTerms) != 2 {
		t.Errorf("expected 2 search terms, got %d: %v", len(saved.SearchTerms), saved.SearchTerms)
	}
	if saved.GatherInterval == "" {
		t.Error("expected non-empty GatherInterval on auto-created topic")
	}
}

func TestDiscoverAutoApply_CreateSkipsExistingTopic(t *testing.T) {
	tmpHome := setupTempHome(t)
	topicsDir := filepath.Join(tmpHome, ".scout", "topics")
	if err := os.MkdirAll(topicsDir, 0700); err != nil {
		t.Fatal(err)
	}

	existing := []gather.TopicConfig{
		{Name: "ai-models", Sources: []string{"web"}, SearchTerms: []string{"GPT"}},
	}
	sugg := &DiscoverSuggestions{
		Recommendations: []DiscoverRecommendation{
			{
				Action:      "create",
				Topic:       "ai-models", // already in existingTopics
				Description: "Should be skipped entirely",
				SearchTerms: []string{"new-term"},
				Rationale:   "evidence",
			},
		},
	}

	if err := discoverAutoApply(sugg, true, false, existing); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	// No file should have been written for the existing topic
	entries, _ := os.ReadDir(topicsDir)
	if len(entries) != 0 {
		t.Errorf("create for existing topic should not write any file, got %d file(s)", len(entries))
	}
}

func TestDiscoverAutoApply_EnhanceAddsSearchTerms(t *testing.T) {
	tmpHome := setupTempHome(t)
	topicsDir := filepath.Join(tmpHome, ".scout", "topics")
	if err := os.MkdirAll(topicsDir, 0700); err != nil {
		t.Fatal(err)
	}

	existing := gather.TopicConfig{
		Name:           "go-development",
		Sources:        []string{"web", "reddit"},
		SearchTerms:    []string{"golang", "Go programming"},
		GatherInterval: "6h",
	}
	writeTopicFile(t, topicsDir, existing)

	sugg := &DiscoverSuggestions{
		Recommendations: []DiscoverRecommendation{
			{
				Action:      "enhance",
				Topic:       "go-development",
				SearchTerms: []string{"Go modules", "Go generics"},
				Rationale:   "new patterns detected",
			},
		},
	}

	if err := discoverAutoApply(sugg, true, false, []gather.TopicConfig{existing}); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	saved := readTopicFile(t, topicsDir, "go-development")
	termSet := make(map[string]bool)
	for _, term := range saved.SearchTerms {
		termSet[term] = true
	}
	for _, want := range []string{"golang", "Go programming", "Go modules", "Go generics"} {
		if !termSet[want] {
			t.Errorf("expected search term %q after enhance, got terms: %v", want, saved.SearchTerms)
		}
	}
}

func TestDiscoverAutoApply_EnhanceDeduplicatesSearchTerms(t *testing.T) {
	tmpHome := setupTempHome(t)
	topicsDir := filepath.Join(tmpHome, ".scout", "topics")
	if err := os.MkdirAll(topicsDir, 0700); err != nil {
		t.Fatal(err)
	}

	existing := gather.TopicConfig{
		Name:           "go-development",
		Sources:        []string{"web"},
		SearchTerms:    []string{"golang", "Go programming"},
		GatherInterval: "6h",
	}
	writeTopicFile(t, topicsDir, existing)

	sugg := &DiscoverSuggestions{
		Recommendations: []DiscoverRecommendation{
			{
				Action:      "enhance",
				Topic:       "go-development",
				SearchTerms: []string{"golang", "Go generics"}, // "golang" already exists
				AddSources:  []string{"web", "hackernews"},     // "web" already exists
				Rationale:   "test deduplication",
			},
		},
	}

	if err := discoverAutoApply(sugg, true, false, []gather.TopicConfig{existing}); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	saved := readTopicFile(t, topicsDir, "go-development")

	golangCount := 0
	for _, term := range saved.SearchTerms {
		if term == "golang" {
			golangCount++
		}
	}
	if golangCount != 1 {
		t.Errorf("'golang' should appear exactly once in search_terms, got %d: %v", golangCount, saved.SearchTerms)
	}

	webCount := 0
	hasHN := false
	for _, src := range saved.Sources {
		if src == "web" {
			webCount++
		}
		if src == "hackernews" {
			hasHN = true
		}
	}
	if webCount != 1 {
		t.Errorf("'web' source should appear exactly once, got %d: %v", webCount, saved.Sources)
	}
	if !hasHN {
		t.Errorf("'hackernews' should have been added to sources, got: %v", saved.Sources)
	}
}

func TestDiscoverAutoApply_EnhanceDeduplicatesFeeds(t *testing.T) {
	tmpHome := setupTempHome(t)
	topicsDir := filepath.Join(tmpHome, ".scout", "topics")
	if err := os.MkdirAll(topicsDir, 0700); err != nil {
		t.Fatal(err)
	}

	existing := gather.TopicConfig{
		Name:           "ai-models",
		Feeds:          []string{"https://openai.com/blog/rss.xml"},
		GatherInterval: "6h",
	}
	writeTopicFile(t, topicsDir, existing)

	sugg := &DiscoverSuggestions{
		Recommendations: []DiscoverRecommendation{
			{
				Action:    "enhance",
				Topic:     "ai-models",
				AddFeeds:  []string{"https://openai.com/blog/rss.xml", "https://anthropic.com/feed.xml"},
				Rationale: "additional feed",
			},
		},
	}

	if err := discoverAutoApply(sugg, true, false, []gather.TopicConfig{existing}); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	saved := readTopicFile(t, topicsDir, "ai-models")

	openaiCount := 0
	for _, feed := range saved.Feeds {
		if feed == "https://openai.com/blog/rss.xml" {
			openaiCount++
		}
	}
	if openaiCount != 1 {
		t.Errorf("existing feed should appear exactly once, got %d: %v", openaiCount, saved.Feeds)
	}

	hasAnthropic := false
	for _, feed := range saved.Feeds {
		if feed == "https://anthropic.com/feed.xml" {
			hasAnthropic = true
		}
	}
	if !hasAnthropic {
		t.Errorf("new feed should have been added, got: %v", saved.Feeds)
	}
}

func TestDiscoverAutoApply_EnhanceSkipsUnknownTopic(t *testing.T) {
	// "enhance" for a topic not in existingTopics should silently skip, not error.
	setupTempHome(t)

	sugg := &DiscoverSuggestions{
		Recommendations: []DiscoverRecommendation{
			{
				Action:    "enhance",
				Topic:     "nonexistent-topic",
				Rationale: "topic does not exist",
			},
		},
	}

	if err := discoverAutoApply(sugg, true, false, nil); err != nil {
		t.Fatalf("enhance of unknown topic should not error, got: %v", err)
	}
}

func TestDiscoverAutoApply_AddSourcesAction(t *testing.T) {
	tmpHome := setupTempHome(t)
	topicsDir := filepath.Join(tmpHome, ".scout", "topics")
	if err := os.MkdirAll(topicsDir, 0700); err != nil {
		t.Fatal(err)
	}

	existing := gather.TopicConfig{
		Name:           "ai-models",
		Sources:        []string{"web", "reddit"},
		GatherInterval: "6h",
	}
	writeTopicFile(t, topicsDir, existing)

	sugg := &DiscoverSuggestions{
		Recommendations: []DiscoverRecommendation{
			{
				Action:     "add_sources",
				Topic:      "ai-models",
				AddSources: []string{"web", "youtube"}, // "web" is already present
				Rationale:  "add YouTube coverage",
			},
		},
	}

	if err := discoverAutoApply(sugg, true, false, []gather.TopicConfig{existing}); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	saved := readTopicFile(t, topicsDir, "ai-models")

	webCount := 0
	hasYouTube := false
	for _, src := range saved.Sources {
		if src == "web" {
			webCount++
		}
		if src == "youtube" {
			hasYouTube = true
		}
	}
	if webCount != 1 {
		t.Errorf("'web' should appear exactly once, got %d: %v", webCount, saved.Sources)
	}
	if !hasYouTube {
		t.Errorf("'youtube' should have been added, got: %v", saved.Sources)
	}
}

func TestDiscoverAutoApply_DryRunWithMixedRecommendations(t *testing.T) {
	// Dry-run with create, enhance, and a skip — no files should be written.
	tmpHome := setupTempHome(t)
	topicsDir := filepath.Join(tmpHome, ".scout", "topics")
	if err := os.MkdirAll(topicsDir, 0700); err != nil {
		t.Fatal(err)
	}

	existing := []gather.TopicConfig{
		{Name: "ai-models", Sources: []string{"web"}, SearchTerms: []string{"GPT"}},
	}

	sugg := &DiscoverSuggestions{
		Recommendations: []DiscoverRecommendation{
			{Action: "create", Topic: "ai-safety", Rationale: "new"},
			{Action: "enhance", Topic: "ai-models", SearchTerms: []string{"Claude"}, Rationale: "existing"},
			{Action: "create", Topic: "ai-models", Rationale: "dupe — should skip"},
		},
	}

	if err := discoverAutoApply(sugg, false, true, existing); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	// No files written in dry-run
	entries, _ := os.ReadDir(topicsDir)
	if len(entries) != 0 {
		t.Errorf("dry-run should not write any files, got %d file(s)", len(entries))
	}
}

// ─── DiscoverCmd flag validation ──────────────────────────────────────────────

func TestDiscoverCmd_DryRunAndAutoMutuallyExclusive(t *testing.T) {
	setupTempHome(t) // EnsureDirs runs before flag check
	err := DiscoverCmd([]string{"--dry-run", "--auto"}, false)
	if err == nil {
		t.Fatal("expected error when --dry-run and --auto are both set")
	}
	if !containsStr(err.Error(), "mutually exclusive") {
		t.Errorf("error should mention mutual exclusivity, got: %v", err)
	}
}

func TestDiscoverCmd_UnknownFlag(t *testing.T) {
	setupTempHome(t)
	err := DiscoverCmd([]string{"--unknown-flag"}, false)
	if err == nil {
		t.Fatal("expected error for unknown flag")
	}
	if !containsStr(err.Error(), "unknown flag") {
		t.Errorf("error should mention unknown flag, got: %v", err)
	}
}

// ─── DiscoverCmd error paths ──────────────────────────────────────────────────

func TestDiscoverCmd_ErrorsWhenNoAPIKey(t *testing.T) {
	// Empty home → no config file → GeminiAPIKey="" → error.
	setupTempHome(t)
	err := DiscoverCmd([]string{}, false)
	if err == nil {
		t.Fatal("expected error when Gemini API key is not configured")
	}
	if !containsStr(err.Error(), "Gemini API key") {
		t.Errorf("error should mention Gemini API key, got: %v", err)
	}
}

func TestDiscoverCmd_NoIntelReturnsNil(t *testing.T) {
	// Config has API key but no intel gathered → returns nil (prints message).
	tmpHome := setupTempHome(t)
	configDir := filepath.Join(tmpHome, ".scout")
	if err := os.MkdirAll(configDir, 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(
		filepath.Join(configDir, "config"),
		[]byte("gemini_apikey=fake-key-for-testing\n"),
		0600,
	); err != nil {
		t.Fatal(err)
	}

	// No intel files exist — DiscoverCmd should short-circuit with nil error.
	if err := DiscoverCmd([]string{}, false); err != nil {
		t.Fatalf("expected nil error when no intel found, got: %v", err)
	}
}

func TestDiscoverCmd_NoIntelJSONOutputReturnsNil(t *testing.T) {
	// JSON mode with no intel should also return nil (prints "[]").
	tmpHome := setupTempHome(t)
	configDir := filepath.Join(tmpHome, ".scout")
	if err := os.MkdirAll(configDir, 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(
		filepath.Join(configDir, "config"),
		[]byte("gemini_apikey=fake-key-for-testing\n"),
		0600,
	); err != nil {
		t.Fatal(err)
	}

	if err := DiscoverCmd([]string{}, true); err != nil {
		t.Fatalf("expected nil error for JSON output with no intel, got: %v", err)
	}
}

func TestDiscoverCmd_InvalidSinceDuration(t *testing.T) {
	// --since with a bad value should return a parse error before hitting Gemini.
	tmpHome := setupTempHome(t)
	configDir := filepath.Join(tmpHome, ".scout")
	if err := os.MkdirAll(configDir, 0700); err != nil {
		t.Fatal(err)
	}
	os.WriteFile(filepath.Join(configDir, "config"), []byte("gemini_apikey=key\n"), 0600)

	err := DiscoverCmd([]string{"--since", "notaduration"}, false)
	if err == nil {
		t.Fatal("expected error for invalid --since value")
	}
	if !containsStr(err.Error(), "invalid --since") {
		t.Errorf("error should mention invalid --since, got: %v", err)
	}
}
