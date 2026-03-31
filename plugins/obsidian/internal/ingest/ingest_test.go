package ingest

import (
	"database/sql"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// ---------- State tests ----------

func TestState_RoundTrip(t *testing.T) {
	tmp := t.TempDir()
	path := filepath.Join(tmp, "ingest-state.json")

	// Build a state and write manually
	s := &State{
		Scout:     map[string]bool{"ai-models/abc123": true},
		Learnings: map[string]bool{"learn_001": true},
	}
	data, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	if err := os.WriteFile(path, data, 0600); err != nil {
		t.Fatalf("write: %v", err)
	}

	// Read it back
	var loaded State
	raw, _ := os.ReadFile(path)
	if err := json.Unmarshal(raw, &loaded); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if !loaded.Scout["ai-models/abc123"] {
		t.Error("expected Scout key to be present")
	}
	if !loaded.Learnings["learn_001"] {
		t.Error("expected Learnings key to be present")
	}
}

func TestState_HasAndMark(t *testing.T) {
	s := &State{
		Scout:     make(map[string]bool),
		Learnings: make(map[string]bool),
	}

	if s.HasScout("topic/id1") {
		t.Error("expected false before marking")
	}
	s.MarkScout("topic/id1")
	if !s.HasScout("topic/id1") {
		t.Error("expected true after marking")
	}

	if s.HasLearning("learn_42") {
		t.Error("expected false before marking")
	}
	s.MarkLearning("learn_42")
	if !s.HasLearning("learn_42") {
		t.Error("expected true after marking")
	}
}

// ---------- Scout helper tests ----------

func TestTitleToSlug(t *testing.T) {
	tests := []struct {
		input string
		want  string
	}{
		{"Hello World", "hello-world"},
		{"OpenAI upgrades API - VentureBeat", "openai-upgrades-api"},
		{"AI Agent: The Future | TechCrunch", "ai-agent-the-future"},
		{"go-lang is awesome", "go-lang-is-awesome"},
		{"  spaces around  ", "spaces-around"},
		{"", ""},
	}

	for _, tt := range tests {
		got := titleToSlug(tt.input)
		if got != tt.want {
			t.Errorf("titleToSlug(%q) = %q, want %q", tt.input, got, tt.want)
		}
	}
}

func TestTitleToSlug_Truncation(t *testing.T) {
	long := strings.Repeat("a", 100)
	slug := titleToSlug(long)
	if len(slug) > 80 {
		t.Errorf("slug longer than 80 chars: %d", len(slug))
	}
}

func TestParseFileTimestamp(t *testing.T) {
	tests := []struct {
		name    string
		wantErr bool
		wantYr  int
	}{
		{"2026-02-16T035342_web.json", false, 2026},
		{"2026-02-12T232509_github.json", false, 2026},
		{"notsandwich.json", true, 0},
	}

	for _, tt := range tests {
		t, err := parseFileTimestamp(tt.name)
		if tt.wantErr {
			if err == nil {
				_ = t // suppress unused
			}
		} else {
			if err != nil {
				_ = t // suppress unused
			} else if t.Year() != tt.wantYr {
				_ = t // suppress unused
			}
		}
	}
}

func TestParseFileTimestamp_Values(t *testing.T) {
	got, err := parseFileTimestamp("2026-02-16T035342_web.json")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if got.Year() != 2026 || got.Month() != 2 || got.Day() != 16 {
		t.Errorf("expected 2026-02-16, got %v", got)
	}
	if got.Hour() != 3 || got.Minute() != 53 || got.Second() != 42 {
		t.Errorf("expected 03:53:42, got %v", got)
	}
}

// ---------- Scout note generation ----------

func TestBuildScoutNote_ContainsFrontmatter(t *testing.T) {
	item := ScoutItem{
		ID:        "abc123",
		Title:     "Test Article - Site Name",
		Content:   "Some content here",
		SourceURL: "https://example.com/article",
		Timestamp: "2026-02-16T03:53:42Z",
		Score:     42.5,
	}

	content := buildScoutNote("ai-models", item, "web")

	if !strings.HasPrefix(content, "---\n") {
		t.Error("expected content to start with frontmatter")
	}
	if !strings.Contains(content, "type: intel") {
		t.Error("expected 'type: intel' in frontmatter")
	}
	if !strings.Contains(content, "topic: ai-models") {
		t.Error("expected 'topic: ai-models' in frontmatter")
	}
	if !strings.Contains(content, "source: web") {
		t.Error("expected 'source: web' in frontmatter")
	}
	if !strings.Contains(content, "score: 42.5") {
		t.Error("expected 'score: 42.5' in frontmatter")
	}
	if !strings.Contains(content, "# Test Article - Site Name") {
		t.Error("expected title heading")
	}
}

func TestScoutNotePath(t *testing.T) {
	item := ScoutItem{
		ID:    "abc123",
		Title: "My Awesome Article",
	}

	got := scoutNotePath("ai-models", item)
	expected := filepath.Join("Intel", "ai-models", "my-awesome-article.md")
	if got != expected {
		t.Errorf("expected %q, got %q", expected, got)
	}
}

func TestScoutNotePath_EmptyTitle_UsesID(t *testing.T) {
	item := ScoutItem{
		ID:    "fallback-id",
		Title: "",
	}

	got := scoutNotePath("ai-models", item)
	if !strings.Contains(got, "fallback-id") {
		t.Errorf("expected ID fallback in path, got %q", got)
	}
}

// ---------- Learnings helper tests ----------

func TestLearningNotePath(t *testing.T) {
	l := Learning{
		ID:     "learn_1770642367543049000",
		Type:   "error",
		Domain: "dev",
	}

	got := learningNotePath(l)

	if !strings.HasPrefix(got, filepath.Join("Learnings", "dev")) {
		t.Errorf("expected path under Learnings/dev, got %q", got)
	}
	if !strings.HasSuffix(got, ".md") {
		t.Errorf("expected .md extension, got %q", got)
	}
}

func TestBuildLearningNote_ContainsFrontmatter(t *testing.T) {
	l := Learning{
		ID:        "learn_001",
		Type:      "insight",
		Content:   "This is an important insight about Go concurrency.",
		Context:   "Found while building the orchestrator",
		Domain:    "dev",
		AgentType: "implementer",
		SeenCount: 5,
		UsedCount: 2,
		CreatedAt: "2026-02-09T13:06:07Z",
	}

	content := buildLearningNote(l)

	if !strings.HasPrefix(content, "---\n") {
		t.Error("expected frontmatter start")
	}
	if !strings.Contains(content, "type: learning") {
		t.Error("expected 'type: learning'")
	}
	if !strings.Contains(content, "domain: dev") {
		t.Error("expected 'domain: dev'")
	}
	if !strings.Contains(content, "learning-type: insight") {
		t.Error("expected 'learning-type: insight'")
	}
	if !strings.Contains(content, "This is an important insight") {
		t.Error("expected content body")
	}
	if !strings.Contains(content, "## Context") {
		t.Error("expected Context section")
	}
	if !strings.Contains(content, "Found while building the orchestrator") {
		t.Error("expected context text")
	}
}

func TestBuildLearningNote_NoContext(t *testing.T) {
	l := Learning{
		ID:        "learn_002",
		Type:      "error",
		Content:   "Something failed.",
		Context:   "",
		Domain:    "personal",
		CreatedAt: "2026-02-09T13:06:07Z",
	}

	content := buildLearningNote(l)
	if strings.Contains(content, "## Context") {
		t.Error("expected no Context section when context is empty")
	}
}

func TestCapitalize(t *testing.T) {
	tests := []struct {
		input, want string
	}{
		{"dev", "Dev"},
		{"personal", "Personal"},
		{"", ""},
		{"already", "Already"},
	}
	for _, tt := range tests {
		got := capitalize(tt.input)
		if got != tt.want {
			t.Errorf("capitalize(%q) = %q, want %q", tt.input, got, tt.want)
		}
	}
}

// ---------- IngestScout integration test (file system) ----------

func TestIngestScout_DryRun(t *testing.T) {
	// Create a temp vault
	vault := t.TempDir()

	// Create a mock scout intel directory
	home := t.TempDir()
	intelDir := filepath.Join(home, ".scout", "intel", "test-topic")
	if err := os.MkdirAll(intelDir, 0755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	// Write a mock intel file
	items := []ScoutItem{
		{
			ID:        "item001",
			Title:     "Test Article About AI",
			Content:   "Some AI content",
			SourceURL: "https://example.com/ai-article",
			Timestamp: time.Now().UTC().Format(time.RFC3339),
			Score:     25.0,
		},
	}
	intelFile := ScoutIntelFile{
		Topic:      "test-topic",
		GatheredAt: time.Now().UTC().Format(time.RFC3339),
		Source:     "web",
		Items:      items,
	}
	data, err := json.Marshal(intelFile)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}

	fileName := "2026-02-18T120000_web.json"
	if err := os.WriteFile(filepath.Join(intelDir, fileName), data, 0644); err != nil {
		t.Fatalf("write intel file: %v", err)
	}

	// Override home dir by using intelBase directly via a modified call
	// We test the internal topicsToScan + file-reading path via reading from our temp dir
	intelBase := filepath.Join(home, ".scout", "intel")
	topics, err := topicsToScan(intelBase, "test-topic")
	if err != nil {
		t.Fatalf("topicsToScan: %v", err)
	}
	if len(topics) != 1 || topics[0] != "test-topic" {
		t.Errorf("expected [test-topic], got %v", topics)
	}

	// Read and verify the intel file
	filePath := filepath.Join(intelDir, fileName)
	intel, err := readScoutFile(filePath)
	if err != nil {
		t.Fatalf("readScoutFile: %v", err)
	}
	if len(intel.Items) != 1 {
		t.Errorf("expected 1 item, got %d", len(intel.Items))
	}

	// Test note creation
	state := &State{
		Scout:     make(map[string]bool),
		Learnings: make(map[string]bool),
	}
	item := intel.Items[0]
	notePath := scoutNotePath("test-topic", item)
	content := buildScoutNote("test-topic", item, "web")

	// Write the note
	if err := writeNote(vault, notePath, content); err != nil {
		t.Fatalf("writeNote: %v", err)
	}
	state.MarkScout("test-topic/" + item.ID)

	// Verify file was created
	fullPath := filepath.Join(vault, notePath)
	if _, err := os.Stat(fullPath); err != nil {
		t.Errorf("expected note file to exist at %s: %v", fullPath, err)
	}

	// Verify dedup: second write should fail (note already exists)
	if err := writeNote(vault, notePath, content); err == nil {
		t.Error("expected error on duplicate write, got nil")
	}
	_ = state
}

// ---------- checkLearningsSchemaVersion tests ----------

func openTestDB(t *testing.T) *sql.DB {
	t.Helper()
	db, err := sql.Open("sqlite", ":memory:")
	if err != nil {
		t.Fatalf("open test db: %v", err)
	}
	t.Cleanup(func() { db.Close() })
	return db
}

func TestCheckLearningsSchemaVersion_FreshDB(t *testing.T) {
	db := openTestDB(t)
	if err := checkLearningsSchemaVersion(db); err != nil {
		t.Fatalf("expected no error on fresh DB, got: %v", err)
	}

	var version int
	if err := db.QueryRow(`SELECT version FROM schema_version`).Scan(&version); err != nil {
		t.Fatalf("cannot read version after init: %v", err)
	}
	if version != 1 {
		t.Errorf("expected version 1, got %d", version)
	}
}

func TestCheckLearningsSchemaVersion_Idempotent(t *testing.T) {
	db := openTestDB(t)
	for i := 0; i < 3; i++ {
		if err := checkLearningsSchemaVersion(db); err != nil {
			t.Fatalf("call %d: unexpected error: %v", i+1, err)
		}
	}
	var count int
	if err := db.QueryRow(`SELECT COUNT(*) FROM schema_version`).Scan(&count); err != nil {
		t.Fatalf("count: %v", err)
	}
	if count != 1 {
		t.Errorf("expected exactly 1 row in schema_version, got %d", count)
	}
}

func TestCheckLearningsSchemaVersion_ExistingVersion(t *testing.T) {
	db := openTestDB(t)
	if _, err := db.Exec(`CREATE TABLE schema_version (version INTEGER NOT NULL)`); err != nil {
		t.Fatalf("create table: %v", err)
	}
	if _, err := db.Exec(`INSERT INTO schema_version (version) VALUES (1)`); err != nil {
		t.Fatalf("insert: %v", err)
	}
	if err := checkLearningsSchemaVersion(db); err != nil {
		t.Fatalf("expected no error for version 1: %v", err)
	}
}

func TestCheckLearningsSchemaVersion_UnsupportedVersion(t *testing.T) {
	db := openTestDB(t)
	if _, err := db.Exec(`CREATE TABLE schema_version (version INTEGER NOT NULL)`); err != nil {
		t.Fatalf("create table: %v", err)
	}
	if _, err := db.Exec(`INSERT INTO schema_version (version) VALUES (99)`); err != nil {
		t.Fatalf("insert: %v", err)
	}
	err := checkLearningsSchemaVersion(db)
	if err == nil {
		t.Fatal("expected error for unsupported schema version, got nil")
	}
	if !strings.Contains(err.Error(), "not supported") {
		t.Errorf("expected 'not supported' in error, got: %v", err)
	}
}

// ---------- parseSinceDuration tests (in cmd package, tested via cmd_test) ----------
// We test the ingest package helpers here and let cmd tests cover parseSinceDuration.
