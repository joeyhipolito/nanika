package cmd

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/index"
	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// mockLLMClassifier is a test double for LLMClassifier.
type mockLLMClassifier struct {
	result LLMClassifyResult
	err    error
}

func (m *mockLLMClassifier) Classify(_ context.Context, _ string) (LLMClassifyResult, error) {
	return m.result, m.err
}

func TestClassifyNoteType(t *testing.T) {
	tests := []struct {
		name     string
		fm       map[string]any
		body     string
		headings []vault.Heading
		want     string
	}{
		{
			name: "preserves existing non-fleeting type",
			fm:   map[string]any{"type": "reference"},
			body: "some content",
			want: "reference",
		},
		{
			name: "reclassifies fleeting with task signals",
			fm:   map[string]any{"type": "fleeting"},
			body: "- [ ] buy milk\n- [ ] call dentist\n",
			want: "task",
		},
		{
			name: "reclassifies fleeting with TODO marker",
			fm:   map[string]any{"type": "fleeting"},
			body: "todo: finish the report\n",
			want: "task",
		},
		{
			name: "reference from source URL",
			fm:   map[string]any{"type": "fleeting", "source": "https://example.com/article"},
			body: "interesting article",
			want: "reference",
		},
		{
			name: "reference from inline URL in body",
			fm:   map[string]any{"type": "fleeting"},
			body: "Check out https://golang.org for docs.",
			want: "reference",
		},
		{
			name: "note from multiple headings",
			fm:   map[string]any{"type": "fleeting"},
			body: "## Background\nSome text.\n## Analysis\nMore text.\n",
			headings: []vault.Heading{
				{Level: 2, Text: "Background"},
				{Level: 2, Text: "Analysis"},
			},
			want: "note",
		},
		{
			name: "defaults to idea",
			fm:   map[string]any{"type": "fleeting"},
			body: "A rough idea about improving the search UI.",
			want: "idea",
		},
		{
			name: "no type in frontmatter → idea",
			fm:   map[string]any{},
			body: "Just a quick thought.",
			want: "idea",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			parsed := &vault.Note{
				Frontmatter: tt.fm,
				Body:        tt.body,
				Headings:    tt.headings,
			}
			got := classifyNoteType(parsed)
			if got != tt.want {
				t.Errorf("classifyNoteType() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestTypeFolder(t *testing.T) {
	tests := []struct {
		noteType string
		want     string
	}{
		{"task", "Tasks"},
		{"reference", "References"},
		{"idea", "Ideas"},
		{"note", "Notes"},
		{"unknown", "Notes"},
		{"", "Notes"},
	}
	for _, tt := range tests {
		got := typeFolder(tt.noteType)
		if got != tt.want {
			t.Errorf("typeFolder(%q) = %q, want %q", tt.noteType, got, tt.want)
		}
	}
}

func TestSlugify(t *testing.T) {
	tests := []struct {
		input string
		want  string
	}{
		{"Hello World", "hello-world"},
		{"Golang Error Handling", "golang-error-handling"},
		{"  spaces   ", "spaces"},
		{"Special: Characters!", "special-characters"},
		{"", "untitled"},
		{"---", "untitled"},
		{"A  B", "a-b"},
	}
	for _, tt := range tests {
		got := slugify(tt.input)
		if got != tt.want {
			t.Errorf("slugify(%q) = %q, want %q", tt.input, got, tt.want)
		}
	}
}

func TestNoteSlug(t *testing.T) {
	tests := []struct {
		name     string
		fromPath string
		fm       map[string]any
		headings []vault.Heading
		want     string
	}{
		{
			name:     "uses frontmatter title",
			fromPath: "Inbox/20260221-143022.md",
			fm:       map[string]any{"title": "My Great Idea"},
			want:     "my-great-idea",
		},
		{
			name:     "uses first H1 heading",
			fromPath: "Inbox/20260221-143022.md",
			fm:       map[string]any{},
			headings: []vault.Heading{{Level: 1, Text: "Article Title"}},
			want:     "article-title",
		},
		{
			name:     "falls back to filename",
			fromPath: "Inbox/20260221-143022.md",
			fm:       map[string]any{},
			want:     "20260221-143022",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			parsed := &vault.Note{
				Frontmatter: tt.fm,
				Headings:    tt.headings,
			}
			got := noteSlug(tt.fromPath, parsed)
			if got != tt.want {
				t.Errorf("noteSlug() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestFormatAgeLabel(t *testing.T) {
	tests := []struct {
		days int
		want string
	}{
		{0, "today"},
		{1, "1 day ago"},
		{5, "5 days ago"},
		{7, "1 week ago"},
		{10, "1 week ago"},
		{14, "2 weeks ago"},
		{30, "1 month ago"},
		{60, "2 months ago"},
	}
	for _, tt := range tests {
		got := formatAgeLabel(tt.days)
		if got != tt.want {
			t.Errorf("formatAgeLabel(%d) = %q, want %q", tt.days, got, tt.want)
		}
	}
}

func TestEnrichSingleNoteFromRows_SortsBySimilarityDescending(t *testing.T) {
	// unit vector helpers: embed notes as simple unit vectors so cosine
	// similarity equals the dot product and is easy to reason about.
	vec := func(v ...float32) []float32 { return v }

	target := "Inbox/target.md"
	notes := []index.NoteRow{
		{Path: target, Embedding: vec(1, 0, 0)},
		// sim ≈ 0.71 — above threshold but lowest of the three candidates
		{Path: "Notes/low.md", Embedding: vec(1, 1, 0)},   // sim = 1/√2 ≈ 0.707
		// sim ≈ 0.89 — middle candidate
		{Path: "Notes/mid.md", Embedding: vec(1, 0.5, 0)}, // sim = 1/√1.25 ≈ 0.894
		// sim = 1.0 — highest candidate
		{Path: "Notes/high.md", Embedding: vec(1, 0, 0)},  // sim = 1.0
		// sim = 0.0 — below threshold, must be excluded
		{Path: "Notes/none.md", Embedding: vec(0, 1, 0)},
	}

	got := enrichSingleNoteFromRows(notes, target)

	if len(got) != 3 {
		t.Fatalf("expected 3 suggestions, got %d", len(got))
	}

	// Verify descending order: high → mid → low
	for i := 1; i < len(got); i++ {
		if got[i].Similarity > got[i-1].Similarity {
			t.Errorf("suggestions not sorted: got[%d].Similarity=%.4f > got[%d].Similarity=%.4f",
				i, got[i].Similarity, i-1, got[i-1].Similarity)
		}
	}

	if got[0].To != "Notes/high.md" {
		t.Errorf("expected highest-similarity note first, got %q", got[0].To)
	}
}

func TestEnrichSingleNoteFromRows_CapsAtFive(t *testing.T) {
	vec := func(v ...float32) []float32 { return v }

	target := "Inbox/target.md"
	notes := []index.NoteRow{
		{Path: target, Embedding: vec(1, 0)},
	}
	// Add 7 candidates all with sim = 1.0 (parallel vectors).
	for i := 0; i < 7; i++ {
		notes = append(notes, index.NoteRow{
			Path:      "Notes/note" + string(rune('a'+i)) + ".md",
			Embedding: vec(1, 0),
		})
	}

	got := enrichSingleNoteFromRows(notes, target)
	if len(got) != 5 {
		t.Errorf("expected at most 5 suggestions, got %d", len(got))
	}
}

func TestNoteTypeEnum(t *testing.T) {
	types := []NoteType{NoteTypeFleeting, NoteTypeTask, NoteTypeReference, NoteTypeIdea, NoteTypeNote}
	for _, nt := range types {
		if !isValidNoteType(nt) {
			t.Errorf("isValidNoteType(%q) = false, want true", nt)
		}
	}
	if isValidNoteType("bogus") {
		t.Error("isValidNoteType(\"bogus\") = true, want false")
	}
}

func TestClassifyWithLLM_HighConfidence(t *testing.T) {
	parsed := &vault.Note{
		Frontmatter: map[string]any{"type": "fleeting"},
		Body:        "Some task-like content",
	}
	llm := &mockLLMClassifier{
		result: LLMClassifyResult{Type: NoteTypeTask, Confidence: 0.9, Entities: []string{"project"}},
	}
	got, entities := classifyWithLLM(context.Background(), parsed, llm)
	if got != "task" {
		t.Errorf("classifyWithLLM() type = %q, want %q", got, "task")
	}
	if len(entities) != 1 || entities[0] != "project" {
		t.Errorf("classifyWithLLM() entities = %v, want [project]", entities)
	}
}

func TestClassifyWithLLM_LowConfidence_FallsBackToRegex(t *testing.T) {
	parsed := &vault.Note{
		Frontmatter: map[string]any{"type": "fleeting"},
		Body:        "- [ ] buy milk\n",
	}
	llm := &mockLLMClassifier{
		result: LLMClassifyResult{Type: NoteTypeIdea, Confidence: 0.3},
	}
	got, entities := classifyWithLLM(context.Background(), parsed, llm)
	// Regex sees a checkbox → "task"
	if got != "task" {
		t.Errorf("classifyWithLLM() type = %q, want %q (regex fallback)", got, "task")
	}
	if len(entities) != 0 {
		t.Errorf("classifyWithLLM() entities = %v, want nil on fallback", entities)
	}
}

func TestClassifyWithLLM_NilClassifier_UsesRegex(t *testing.T) {
	parsed := &vault.Note{
		Frontmatter: map[string]any{"type": "fleeting"},
		Body:        "https://golang.org is great",
	}
	got, entities := classifyWithLLM(context.Background(), parsed, nil)
	if got != "reference" {
		t.Errorf("classifyWithLLM() type = %q, want %q", got, "reference")
	}
	if len(entities) != 0 {
		t.Errorf("classifyWithLLM() entities should be nil without LLM, got %v", entities)
	}
}

func TestClassifyWithLLM_LLMError_FallsBackToRegex(t *testing.T) {
	parsed := &vault.Note{
		Frontmatter: map[string]any{"type": "fleeting"},
		Body:        "a simple idea",
	}
	llm := &mockLLMClassifier{err: context.DeadlineExceeded}
	got, _ := classifyWithLLM(context.Background(), parsed, llm)
	if got != "idea" {
		t.Errorf("classifyWithLLM() on error = %q, want %q", got, "idea")
	}
}

func TestClassifyWithLLM_FleetingFromLLM_FallsBackToRegex(t *testing.T) {
	// LLM returning "fleeting" means it has no opinion — defer to regex.
	parsed := &vault.Note{
		Frontmatter: map[string]any{"type": "fleeting"},
		Body:        "- [ ] send invoice",
	}
	llm := &mockLLMClassifier{
		result: LLMClassifyResult{Type: NoteTypeFleeting, Confidence: 0.8},
	}
	got, _ := classifyWithLLM(context.Background(), parsed, llm)
	if got != "task" {
		t.Errorf("classifyWithLLM() fleeting LLM result = %q, want %q (regex)", got, "task")
	}
}

func TestClassifyWithLLM_InvalidTypeFromLLM_FallsBackToRegex(t *testing.T) {
	parsed := &vault.Note{
		Frontmatter: map[string]any{"type": "fleeting"},
		Body:        "a simple idea",
	}
	llm := &mockLLMClassifier{
		result: LLMClassifyResult{Type: "unknown-type", Confidence: 0.95},
	}
	got, _ := classifyWithLLM(context.Background(), parsed, llm)
	if got != "idea" {
		t.Errorf("classifyWithLLM() invalid LLM type = %q, want %q (regex)", got, "idea")
	}
}

func TestMatchEntitiesAgainstVault(t *testing.T) {
	vaultDir := t.TempDir()
	notesDir := filepath.Join(vaultDir, "Notes")
	if err := os.MkdirAll(notesDir, 0755); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"golang-error-handling.md", "concurrency.md", "unrelated.md"} {
		if err := os.WriteFile(filepath.Join(notesDir, name), []byte("# "+name), 0644); err != nil {
			t.Fatal(err)
		}
	}

	entities := []string{"Golang Error Handling", "Concurrency", "NonExistent"}
	matches := matchEntitiesAgainstVault(vaultDir, entities)

	if len(matches) != 2 {
		t.Fatalf("matchEntitiesAgainstVault() = %v (len %d), want 2 matches", matches, len(matches))
	}
	// Order may vary; check presence.
	matchSet := make(map[string]bool)
	for _, m := range matches {
		matchSet[m] = true
	}
	for _, want := range []string{"golang-error-handling", "concurrency"} {
		if !matchSet[want] {
			t.Errorf("matchEntitiesAgainstVault() missing %q, got %v", want, matches)
		}
	}
}

func TestMatchEntitiesAgainstVault_Empty(t *testing.T) {
	got := matchEntitiesAgainstVault(t.TempDir(), nil)
	if got != nil {
		t.Errorf("expected nil for empty entities, got %v", got)
	}
}

func TestAppendToCanonical(t *testing.T) {
	f, err := os.CreateTemp(t.TempDir(), "canonical-*.md")
	if err != nil {
		t.Fatal(err)
	}
	_, _ = f.WriteString("# Existing Note\n\nOriginal content.\n")
	f.Close()

	now := time.Date(2026, 3, 17, 0, 0, 0, 0, time.UTC)
	if err := appendToCanonical(f.Name(), "New idea here.", now); err != nil {
		t.Fatalf("appendToCanonical() error: %v", err)
	}

	data, _ := os.ReadFile(f.Name())
	content := string(data)

	if !strings.Contains(content, "Original content.") {
		t.Error("appendToCanonical() clobbered existing content")
	}
	if !strings.Contains(content, "New idea here.") {
		t.Error("appendToCanonical() did not append new body")
	}
	if !strings.Contains(content, "2026-03-17") {
		t.Error("appendToCanonical() missing date stamp")
	}
	if !strings.Contains(content, "---") {
		t.Error("appendToCanonical() missing divider")
	}
}

func TestAppendToCanonical_NoTrailingNewline(t *testing.T) {
	f, err := os.CreateTemp(t.TempDir(), "canonical-*.md")
	if err != nil {
		t.Fatal(err)
	}
	// Write content without trailing newline.
	_, _ = f.WriteString("# Note")
	f.Close()

	now := time.Date(2026, 3, 17, 0, 0, 0, 0, time.UTC)
	if err := appendToCanonical(f.Name(), "appended", now); err != nil {
		t.Fatalf("appendToCanonical() error: %v", err)
	}

	data, _ := os.ReadFile(f.Name())
	if !strings.Contains(string(data), "appended") {
		t.Error("content not appended")
	}
}

func TestContainsStr(t *testing.T) {
	if !containsStr([]string{"a", "b", "c"}, "b") {
		t.Error("expected true for existing element")
	}
	if containsStr([]string{"a", "b"}, "z") {
		t.Error("expected false for missing element")
	}
	if containsStr(nil, "x") {
		t.Error("expected false for nil slice")
	}
}

func TestBuildTriagedContent_PreservesCustomFrontmatter(t *testing.T) {
	now := time.Date(2026, 3, 18, 0, 0, 0, 0, time.UTC)
	parsed := &vault.Note{
		Frontmatter: map[string]any{
			"title":        "My Note",
			"created":      "2026-03-01",
			"source":       "https://example.com/article",
			"tags":         []string{"go", "backend"},
			"context-set":  "work-project",
			"summary":      "A key insight from reading",
			"custom-field": "user-defined-value",
		},
		Body: "Some note body.",
	}

	result := buildTriagedContent(parsed, "reference", nil, now)

	// Triage fields must be set.
	if !strings.Contains(result, "type: reference") {
		t.Error("missing type: reference")
	}
	if !strings.Contains(result, "status: processed") {
		t.Error("missing status: processed")
	}
	if !strings.Contains(result, "triaged: 2026-03-18") {
		t.Error("missing triaged date")
	}

	// Standard fields must be preserved.
	if !strings.Contains(result, "title: My Note") {
		t.Error("title not preserved")
	}
	if !strings.Contains(result, "created: 2026-03-01") {
		t.Error("created not preserved")
	}
	if !strings.Contains(result, `source: "https://example.com/article"`) {
		t.Error("source not preserved")
	}
	if !strings.Contains(result, "- go") || !strings.Contains(result, "- backend") {
		t.Error("tags not preserved")
	}

	// Custom frontmatter keys must survive triage.
	if !strings.Contains(result, "context-set: work-project") {
		t.Error("context-set custom key not preserved")
	}
	if !strings.Contains(result, "summary: A key insight from reading") {
		t.Error("summary custom key not preserved")
	}
	if !strings.Contains(result, "custom-field: user-defined-value") {
		t.Error("custom-field custom key not preserved")
	}

	// Body must be present.
	if !strings.Contains(result, "Some note body.") {
		t.Error("body not preserved")
	}
}

func TestBuildTriagedContent_NoCustomKeys_Baseline(t *testing.T) {
	now := time.Date(2026, 3, 18, 0, 0, 0, 0, time.UTC)
	parsed := &vault.Note{
		Frontmatter: map[string]any{
			"title":   "Simple Note",
			"created": "2026-03-10",
			"type":    "fleeting",
		},
		Body: "Just a thought.",
	}

	result := buildTriagedContent(parsed, "idea", []string{"related-note"}, now)

	if !strings.Contains(result, "type: idea") {
		t.Error("type not updated to idea")
	}
	if !strings.Contains(result, "status: processed") {
		t.Error("status not set")
	}
	if !strings.Contains(result, "## Related Notes") {
		t.Error("Related Notes section missing")
	}
	if !strings.Contains(result, "[[related-note]]") {
		t.Error("wikilink not added")
	}
}

func TestFrontmatterString(t *testing.T) {
	fm := map[string]any{
		"type":   "fleeting",
		"source": "https://example.com",
		"count":  42,
	}

	if got := frontmatterString(fm, "type"); got != "fleeting" {
		t.Errorf("expected %q, got %q", "fleeting", got)
	}
	if got := frontmatterString(fm, "missing"); got != "" {
		t.Errorf("expected empty string for missing key, got %q", got)
	}
	// Non-string value returns empty string (type assertion fails gracefully).
	if got := frontmatterString(fm, "count"); got != "" {
		t.Errorf("expected empty string for non-string value, got %q", got)
	}
}
