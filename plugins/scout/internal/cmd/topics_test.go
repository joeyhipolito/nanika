package cmd

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/joeyhipolito/nanika-scout/internal/gather"
)

// ─── TopicsCmd list ───────────────────────────────────────────────────────────

func TestTopicsCmd_EmptyDirMarkdown(t *testing.T) {
	setupTempHome(t)
	output, err := captureOutput(func() error {
		return TopicsCmd(nil, false)
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(output, "No topics configured") {
		t.Errorf("expected 'No topics configured', got: %s", output)
	}
}

func TestTopicsCmd_EmptyDirJSON(t *testing.T) {
	setupTempHome(t)
	output, err := captureOutput(func() error {
		return TopicsCmd(nil, true)
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if strings.TrimSpace(output) != "[]" {
		t.Errorf("expected '[]', got: %s", output)
	}
}

func TestTopicsCmd_ListMarkdown(t *testing.T) {
	home := setupTempHome(t)
	topicsDir := filepath.Join(home, ".scout", "topics")
	if err := os.MkdirAll(topicsDir, 0700); err != nil {
		t.Fatal(err)
	}
	writeTopicFile(t, topicsDir, gather.TopicConfig{
		Name:        "go-news",
		Description: "Go ecosystem news",
		Sources:     []string{"hackernews", "reddit"},
		SearchTerms: []string{"golang", "go programming"},
	})

	output, err := captureOutput(func() error {
		return TopicsCmd(nil, false)
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(output, "go-news") {
		t.Errorf("expected topic name in output, got: %s", output)
	}
	if !strings.Contains(output, "Topics (1)") {
		t.Errorf("expected 'Topics (1)' header, got: %s", output)
	}
}

func TestTopicsCmd_ListJSON(t *testing.T) {
	home := setupTempHome(t)
	topicsDir := filepath.Join(home, ".scout", "topics")
	if err := os.MkdirAll(topicsDir, 0700); err != nil {
		t.Fatal(err)
	}
	writeTopicFile(t, topicsDir, gather.TopicConfig{
		Name:        "json-topic",
		Sources:     []string{"hackernews"},
		SearchTerms: []string{"go"},
	})

	output, err := captureOutput(func() error {
		return TopicsCmd(nil, true)
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	var topics []gather.TopicConfig
	if err := json.Unmarshal([]byte(output), &topics); err != nil {
		t.Fatalf("expected valid JSON, got: %s\nerror: %v", output, err)
	}
	if len(topics) != 1 {
		t.Errorf("expected 1 topic, got %d", len(topics))
	}
	if topics[0].Name != "json-topic" {
		t.Errorf("expected name 'json-topic', got %s", topics[0].Name)
	}
}

func TestTopicsCmd_ListMultipleTopics(t *testing.T) {
	home := setupTempHome(t)
	topicsDir := filepath.Join(home, ".scout", "topics")
	if err := os.MkdirAll(topicsDir, 0700); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"alpha", "beta", "gamma"} {
		writeTopicFile(t, topicsDir, gather.TopicConfig{
			Name:    name,
			Sources: []string{"hackernews"},
		})
	}

	output, err := captureOutput(func() error {
		return TopicsCmd(nil, false)
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(output, "Topics (3)") {
		t.Errorf("expected 'Topics (3)', got: %s", output)
	}
}

// ─── topics add ──────────────────────────────────────────────────────────────

func TestTopicsAdd_NoName(t *testing.T) {
	setupTempHome(t)
	err := topicsAddCmd(nil)
	if err == nil {
		t.Fatal("expected error when name is missing")
	}
	if !strings.Contains(err.Error(), "topic name required") {
		t.Errorf("expected 'topic name required' error, got: %v", err)
	}
}

func TestTopicsAdd_UnknownSource(t *testing.T) {
	setupTempHome(t)
	err := topicsAddCmd([]string{"my-topic", "--sources", "fakesource"})
	if err == nil {
		t.Fatal("expected error for unknown source")
	}
	if !strings.Contains(err.Error(), "unknown source") {
		t.Errorf("expected 'unknown source' error, got: %v", err)
	}
}

func TestTopicsAdd_ValidTopic(t *testing.T) {
	home := setupTempHome(t)
	err := topicsAddCmd([]string{"new-topic", "--sources", "hackernews", "--terms", "golang,testing"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	topicPath := filepath.Join(home, ".scout", "topics", "new-topic.json")
	if _, err := os.Stat(topicPath); os.IsNotExist(err) {
		t.Fatalf("expected topic file at %s", topicPath)
	}

	data, _ := os.ReadFile(topicPath)
	var topic gather.TopicConfig
	if err := json.Unmarshal(data, &topic); err != nil {
		t.Fatalf("topic file is not valid JSON: %v", err)
	}
	if topic.Name != "new-topic" {
		t.Errorf("expected name 'new-topic', got %s", topic.Name)
	}
	if len(topic.SearchTerms) != 2 {
		t.Errorf("expected 2 search terms, got %d: %v", len(topic.SearchTerms), topic.SearchTerms)
	}
}

func TestTopicsAdd_AllFlags(t *testing.T) {
	home := setupTempHome(t)
	args := []string{
		"full-topic",
		"--sources", "rss,github,hackernews",
		"--terms", "golang",
		"--feeds", "https://go.dev/blog/feed.atom",
		"--github-queries", "language:go stars:>100",
		"--reddit-subs", "golang",
		"--devto-tags", "go",
		"--lobsters-tags", "go",
		"--youtube-channels", "UCx9QVEApa5BKLw9r8cnOFEA",
		"--arxiv-categories", "cs.LG",
		"--description", "A full test topic",
	}
	if err := topicsAddCmd(args); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	topicPath := filepath.Join(home, ".scout", "topics", "full-topic.json")
	data, _ := os.ReadFile(topicPath)
	var topic gather.TopicConfig
	json.Unmarshal(data, &topic)

	if topic.Description != "A full test topic" {
		t.Errorf("expected description, got %q", topic.Description)
	}
	if len(topic.Feeds) != 1 {
		t.Errorf("expected 1 feed, got %d", len(topic.Feeds))
	}
	if len(topic.GitHubQueries) != 1 {
		t.Errorf("expected 1 github query, got %d", len(topic.GitHubQueries))
	}
}

func TestTopicsAdd_AlreadyExists(t *testing.T) {
	home := setupTempHome(t)
	topicsDir := filepath.Join(home, ".scout", "topics")
	if err := os.MkdirAll(topicsDir, 0700); err != nil {
		t.Fatal(err)
	}
	writeTopicFile(t, topicsDir, gather.TopicConfig{
		Name:    "existing-topic",
		Sources: []string{"hackernews"},
	})

	err := topicsAddCmd([]string{"existing-topic"})
	if err == nil {
		t.Fatal("expected error for duplicate topic")
	}
	if !strings.Contains(err.Error(), "already exists") {
		t.Errorf("expected 'already exists' error, got: %v", err)
	}
}

func TestTopicsAdd_DefaultSearchTermsToName(t *testing.T) {
	home := setupTempHome(t)
	if err := topicsAddCmd([]string{"my-special-topic", "--sources", "hackernews"}); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	topicPath := filepath.Join(home, ".scout", "topics", "my-special-topic.json")
	data, _ := os.ReadFile(topicPath)
	var topic gather.TopicConfig
	json.Unmarshal(data, &topic)
	if len(topic.SearchTerms) != 1 || topic.SearchTerms[0] != "my-special-topic" {
		t.Errorf("expected search terms to default to topic name, got: %v", topic.SearchTerms)
	}
}

func TestTopicsAdd_RSSWarnWhenNoFeeds(t *testing.T) {
	setupTempHome(t)

	r, w, _ := os.Pipe()
	oldStderr := os.Stderr
	os.Stderr = w

	_, addErr := captureOutput(func() error {
		return topicsAddCmd([]string{"rss-topic", "--sources", "rss"})
	})

	w.Close()
	os.Stderr = oldStderr
	buf := make([]byte, 4096)
	n, _ := r.Read(buf)
	stderr := string(buf[:n])

	if addErr != nil {
		t.Fatalf("expected success (warning not error), got: %v", addErr)
	}
	if !strings.Contains(stderr, "Warning") {
		t.Errorf("expected warning about missing feeds on stderr, got: %q", stderr)
	}
}

func TestTopicsAdd_PodcastWarnWhenNoFeeds(t *testing.T) {
	setupTempHome(t)

	r, w, _ := os.Pipe()
	oldStderr := os.Stderr
	os.Stderr = w

	_, addErr := captureOutput(func() error {
		return topicsAddCmd([]string{"podcast-topic", "--sources", "podcast"})
	})

	w.Close()
	os.Stderr = oldStderr
	buf := make([]byte, 4096)
	n, _ := r.Read(buf)
	stderr := string(buf[:n])

	if addErr != nil {
		t.Fatalf("expected success (warning not error), got: %v", addErr)
	}
	if !strings.Contains(stderr, "Warning") {
		t.Errorf("expected warning about missing podcast feeds on stderr, got: %q", stderr)
	}
}

func TestTopicsAdd_UnknownFlag(t *testing.T) {
	setupTempHome(t)
	err := topicsAddCmd([]string{"my-topic", "--unknown-flag", "value"})
	if err == nil {
		t.Fatal("expected error for unknown flag")
	}
}

func TestTopicsAdd_FlagMissingValue(t *testing.T) {
	tests := []struct {
		name string
		args []string
	}{
		{"sources", []string{"t", "--sources"}},
		{"terms", []string{"t", "--terms"}},
		{"feeds", []string{"t", "--feeds"}},
		{"description", []string{"t", "--description"}},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			setupTempHome(t)
			if err := topicsAddCmd(tc.args); err == nil {
				t.Errorf("expected error when --%s has no value", tc.name)
			}
		})
	}
}

func TestTopicsAdd_ValidSources(t *testing.T) {
	validSources := []string{
		"rss", "github", "reddit", "substack", "medium",
		"hackernews", "googlenews", "devto", "lobsters", "youtube",
		"arxiv", "bluesky", "producthunt", "github-trending",
	}
	for _, src := range validSources {
		t.Run(src, func(t *testing.T) {
			setupTempHome(t)
			// Each call in its own home so there's no collision
			err := topicsAddCmd([]string{"test-" + src, "--sources", src})
			if err != nil {
				t.Errorf("source %q should be valid, got error: %v", src, err)
			}
		})
	}
}

// ─── topics remove ───────────────────────────────────────────────────────────

func TestTopicsRemove_TopicExists(t *testing.T) {
	home := setupTempHome(t)
	topicsDir := filepath.Join(home, ".scout", "topics")
	if err := os.MkdirAll(topicsDir, 0700); err != nil {
		t.Fatal(err)
	}
	writeTopicFile(t, topicsDir, gather.TopicConfig{
		Name:    "to-remove",
		Sources: []string{"hackernews"},
	})

	if err := topicsRemoveCmd([]string{"to-remove"}); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	topicPath := filepath.Join(topicsDir, "to-remove.json")
	if _, err := os.Stat(topicPath); !os.IsNotExist(err) {
		t.Error("expected topic file to be deleted")
	}
}

func TestTopicsRemove_NoName(t *testing.T) {
	setupTempHome(t)
	err := topicsRemoveCmd(nil)
	if err == nil {
		t.Fatal("expected error when name is missing")
	}
	if !strings.Contains(err.Error(), "topic name required") {
		t.Errorf("expected 'topic name required', got: %v", err)
	}
}

func TestTopicsRemove_TopicNotFound(t *testing.T) {
	setupTempHome(t)
	err := topicsRemoveCmd([]string{"nonexistent-topic"})
	if err == nil {
		t.Fatal("expected error for nonexistent topic")
	}
	if !strings.Contains(err.Error(), "not found") {
		t.Errorf("expected 'not found' error, got: %v", err)
	}
}

// ─── topics preset ───────────────────────────────────────────────────────────

func TestTopicsPreset_NoArg_ListsPresets(t *testing.T) {
	setupTempHome(t)
	output, err := captureOutput(func() error {
		return topicsPresetCmd(nil)
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(output, "Available presets") {
		t.Errorf("expected preset list, got: %s", output)
	}
	if !strings.Contains(output, "ai-models") {
		t.Errorf("expected ai-models in preset list, got: %s", output)
	}
}

func TestTopicsPreset_UnknownPreset(t *testing.T) {
	setupTempHome(t)
	err := topicsPresetCmd([]string{"not-a-preset"})
	if err == nil {
		t.Fatal("expected error for unknown preset")
	}
	if !strings.Contains(err.Error(), "unknown preset") {
		t.Errorf("expected 'unknown preset' error, got: %v", err)
	}
}

func TestTopicsPreset_AIModels(t *testing.T) {
	home := setupTempHome(t)
	if err := topicsPresetCmd([]string{"ai-models"}); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	topicPath := filepath.Join(home, ".scout", "topics", "ai-models.json")
	if _, err := os.Stat(topicPath); os.IsNotExist(err) {
		t.Error("expected ai-models topic file to be created")
	}
}

func TestTopicsPreset_GoDevPreset(t *testing.T) {
	home := setupTempHome(t)
	if err := topicsPresetCmd([]string{"go-development"}); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	topicPath := filepath.Join(home, ".scout", "topics", "go-development.json")
	if _, err := os.Stat(topicPath); os.IsNotExist(err) {
		t.Error("expected go-development topic file to be created")
	}
}

func TestTopicsPreset_AIAll(t *testing.T) {
	home := setupTempHome(t)
	if err := topicsPresetCmd([]string{"ai-all"}); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	for _, name := range presetOrder {
		topicPath := filepath.Join(home, ".scout", "topics", name+".json")
		if _, err := os.Stat(topicPath); os.IsNotExist(err) {
			t.Errorf("expected preset topic %q to be created", name)
		}
	}
}

func TestTopicsPreset_DevAll(t *testing.T) {
	home := setupTempHome(t)
	if err := topicsPresetCmd([]string{"dev-all"}); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	for _, name := range nonAIPresetOrder {
		topicPath := filepath.Join(home, ".scout", "topics", name+".json")
		if _, err := os.Stat(topicPath); os.IsNotExist(err) {
			t.Errorf("expected preset topic %q to be created", name)
		}
	}
}

func TestTopicsPreset_All(t *testing.T) {
	home := setupTempHome(t)
	if err := topicsPresetCmd([]string{"all"}); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	allNames := append(presetOrder, nonAIPresetOrder...)
	for _, name := range allNames {
		topicPath := filepath.Join(home, ".scout", "topics", name+".json")
		if _, err := os.Stat(topicPath); os.IsNotExist(err) {
			t.Errorf("expected preset topic %q to be created by 'all'", name)
		}
	}
}

func TestTopicsPreset_AlreadyInstalled_Idempotent(t *testing.T) {
	home := setupTempHome(t)
	// First install
	if err := topicsPresetCmd([]string{"ai-models"}); err != nil {
		t.Fatalf("first install failed: %v", err)
	}
	// Second install should succeed silently (skip, not error)
	if err := topicsPresetCmd([]string{"ai-models"}); err != nil {
		t.Fatalf("second install (idempotent) failed: %v", err)
	}
	// File should still exist with original content
	topicPath := filepath.Join(home, ".scout", "topics", "ai-models.json")
	if _, err := os.Stat(topicPath); os.IsNotExist(err) {
		t.Error("topic file should still exist after second install")
	}
}

func TestTopicsPreset_ContentIsValid(t *testing.T) {
	home := setupTempHome(t)
	if err := topicsPresetCmd([]string{"ai-models"}); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	topicPath := filepath.Join(home, ".scout", "topics", "ai-models.json")
	data, err := os.ReadFile(topicPath)
	if err != nil {
		t.Fatalf("read topic file: %v", err)
	}
	var topic gather.TopicConfig
	if err := json.Unmarshal(data, &topic); err != nil {
		t.Fatalf("preset file is not valid JSON: %v", err)
	}
	if topic.Name != "ai-models" {
		t.Errorf("expected name 'ai-models', got %s", topic.Name)
	}
	if len(topic.Sources) == 0 {
		t.Error("preset should have at least one source")
	}
	if len(topic.SearchTerms) == 0 {
		t.Error("preset should have at least one search term")
	}
	if topic.GatherInterval == "" {
		t.Error("preset should have a gather interval")
	}
}

// ─── parseDuration (in intel.go) ─────────────────────────────────────────────

func TestParseDuration(t *testing.T) {
	tests := []struct {
		input   string
		want    string // expected duration as string for comparison
		wantErr bool
	}{
		{"24h", "24h0m0s", false},
		{"1h", "1h0m0s", false},
		{"30m", "30m0s", false},
		{"7d", "168h0m0s", false},  // 7 * 24h
		{"14d", "336h0m0s", false}, // 14 * 24h
		{"1d", "24h0m0s", false},
		{"0d", "0s", false},
		{"2w", "336h0m0s", false}, // 2 * 7 * 24h
		{"1w", "168h0m0s", false},
		{"", "", true},        // empty string → error
		{"7x", "", true},      // invalid unit
		{"notaduration", "", true},
		{"abc", "", true},
		{" 7d ", "168h0m0s", false}, // trimmed
	}

	for _, tc := range tests {
		t.Run(tc.input, func(t *testing.T) {
			got, err := parseDuration(tc.input)
			if tc.wantErr {
				if err == nil {
					t.Errorf("parseDuration(%q) want error, got nil (duration: %v)", tc.input, got)
				}
				return
			}
			if err != nil {
				t.Fatalf("parseDuration(%q) unexpected error: %v", tc.input, err)
			}
			if got.String() != tc.want {
				t.Errorf("parseDuration(%q) = %v, want %v", tc.input, got, tc.want)
			}
		})
	}
}

func TestParseDuration_DaysBoundary(t *testing.T) {
	// Verify exact day multiplication is correct
	got, err := parseDuration("30d")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	wantHours := float64(30 * 24)
	if got.Hours() != wantHours {
		t.Errorf("30d should be %v hours, got %v", wantHours, got.Hours())
	}
}
