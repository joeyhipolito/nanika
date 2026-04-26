package cmd

import (
	"path/filepath"
	"testing"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/index"
)

// openResurfaceTestStore creates a temporary SQLite store for resurface tests.
func openResurfaceTestStore(t *testing.T) *index.Store {
	t.Helper()
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "resurface_test.db")
	store, err := index.Open(dbPath)
	if err != nil {
		t.Fatalf("Open failed: %v", err)
	}
	return store
}

func TestExcerptBody(t *testing.T) {
	tests := []struct {
		name   string
		body   string
		maxLen int
		want   string
	}{
		{
			name:   "short body returned as-is",
			body:   "short text",
			maxLen: 200,
			want:   "short text",
		},
		{
			name:   "long body truncated with ellipsis",
			body:   "one two three four five six seven eight nine ten eleven twelve",
			maxLen: 20,
			want:   "one two three four…",
		},
		{
			name:   "exactly maxLen returned as-is",
			body:   "hello world",
			maxLen: 11,
			want:   "hello world",
		},
		{
			name:   "leading/trailing whitespace trimmed",
			body:   "  hello  ",
			maxLen: 200,
			want:   "hello",
		},
		{
			name:   "empty body returns empty",
			body:   "",
			maxLen: 200,
			want:   "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := excerptBody(tt.body, tt.maxLen)
			if got != tt.want {
				t.Errorf("excerptBody() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestResurfaceMode(t *testing.T) {
	if got := resurfaceMode(true); got != "random" {
		t.Errorf("resurfaceMode(true) = %q, want %q", got, "random")
	}
	if got := resurfaceMode(false); got != "query" {
		t.Errorf("resurfaceMode(false) = %q, want %q", got, "query")
	}
}

func TestNoteRowsToResurfaceResults(t *testing.T) {
	now := time.Unix(1_800_000_000, 0) // fixed reference time
	rows := []index.NoteRow{
		{
			Path:    "Ideas/old-idea.md",
			Title:   "Old Idea",
			Body:    "This is an old idea about something interesting.",
			ModTime: now.Unix() - 14*86400, // 14 days ago
		},
		{
			Path:    "Notes/meeting.md",
			Title:   "",
			Body:    "",
			ModTime: now.Unix() - 30*86400, // 30 days ago
		},
	}

	results := noteRowsToResurfaceResults(rows, now)

	if len(results) != 2 {
		t.Fatalf("got %d results, want 2", len(results))
	}

	if results[0].Path != "Ideas/old-idea.md" {
		t.Errorf("results[0].Path = %q, want %q", results[0].Path, "Ideas/old-idea.md")
	}
	if results[0].Title != "Old Idea" {
		t.Errorf("results[0].Title = %q, want %q", results[0].Title, "Old Idea")
	}
	if results[0].AgeDays != 14 {
		t.Errorf("results[0].AgeDays = %d, want 14", results[0].AgeDays)
	}
	if results[0].Snippet == "" {
		t.Errorf("results[0].Snippet should not be empty")
	}
	if results[1].AgeDays != 30 {
		t.Errorf("results[1].AgeDays = %d, want 30", results[1].AgeDays)
	}
	if results[1].Snippet != "" {
		t.Errorf("results[1].Snippet should be empty for empty body, got %q", results[1].Snippet)
	}
}

func TestResurfaceAgeFiltering(t *testing.T) {
	// Verify that only notes older than cutoff are included, using real DB.
	store := openResurfaceTestStore(t)
	defer store.Close()

	now := time.Now()
	cutoff := now.Add(-7 * 24 * time.Hour).Unix()

	// Insert notes: one old (14 days), one recent (3 days), one ancient (60 days).
	notes := []index.NoteRow{
		{Path: "old/note.md", Title: "Old Note", Body: "old content", ModTime: now.Add(-14 * 24 * time.Hour).Unix()},
		{Path: "recent/note.md", Title: "Recent Note", Body: "recent content", ModTime: now.Add(-3 * 24 * time.Hour).Unix()},
		{Path: "ancient/note.md", Title: "Ancient Note", Body: "ancient content", ModTime: now.Add(-60 * 24 * time.Hour).Unix()},
	}
	for i := range notes {
		if err := store.UpsertNote(&notes[i]); err != nil {
			t.Fatalf("UpsertNote failed: %v", err)
		}
	}

	rows, err := store.RandomOldNotes(cutoff, 10)
	if err != nil {
		t.Fatalf("RandomOldNotes failed: %v", err)
	}

	// Should include old (14d) and ancient (60d) but not recent (3d).
	paths := make(map[string]bool)
	for _, r := range rows {
		paths[r.Path] = true
	}

	if paths["recent/note.md"] {
		t.Error("recent note (3 days old) should not be returned by RandomOldNotes")
	}
	if !paths["old/note.md"] {
		t.Error("old note (14 days old) should be returned by RandomOldNotes")
	}
	if !paths["ancient/note.md"] {
		t.Error("ancient note (60 days old) should be returned by RandomOldNotes")
	}
}

func TestGetModTimes(t *testing.T) {
	store := openResurfaceTestStore(t)
	defer store.Close()

	rows := []index.NoteRow{
		{Path: "a.md", Title: "A", ModTime: 1000},
		{Path: "b.md", Title: "B", ModTime: 2000},
	}
	for i := range rows {
		if err := store.UpsertNote(&rows[i]); err != nil {
			t.Fatalf("UpsertNote failed: %v", err)
		}
	}

	modTimes, err := store.GetModTimes([]string{"a.md", "b.md", "missing.md"})
	if err != nil {
		t.Fatalf("GetModTimes failed: %v", err)
	}

	if modTimes["a.md"] != 1000 {
		t.Errorf("a.md mod_time = %d, want 1000", modTimes["a.md"])
	}
	if modTimes["b.md"] != 2000 {
		t.Errorf("b.md mod_time = %d, want 2000", modTimes["b.md"])
	}
	if _, ok := modTimes["missing.md"]; ok {
		t.Error("missing.md should not appear in mod_times map")
	}
}

func TestGetModTimesEmpty(t *testing.T) {
	store := openResurfaceTestStore(t)
	defer store.Close()

	modTimes, err := store.GetModTimes([]string{})
	if err != nil {
		t.Fatalf("GetModTimes(empty) failed: %v", err)
	}
	if len(modTimes) != 0 {
		t.Errorf("expected empty map, got %v", modTimes)
	}
}

func TestResurfaceParseDuration(t *testing.T) {
	// parseSinceDuration is shared across the package; verify it handles resurface values.
	tests := []struct {
		input   string
		wantErr bool
		wantD   time.Duration
	}{
		{"7d", false, 7 * 24 * time.Hour},
		{"14d", false, 14 * 24 * time.Hour},
		{"24h", false, 24 * time.Hour},
		{"2w", false, 14 * 24 * time.Hour},
		{"bad", true, 0},
		{"", false, 0}, // empty string is treated as "no filter" (returns 0, nil)
	}

	for _, tt := range tests {
		t.Run(tt.input, func(t *testing.T) {
			got, err := parseSinceDuration(tt.input)
			if (err != nil) != tt.wantErr {
				t.Errorf("parseSinceDuration(%q) error = %v, wantErr %v", tt.input, err, tt.wantErr)
			}
			if !tt.wantErr && got != tt.wantD {
				t.Errorf("parseSinceDuration(%q) = %v, want %v", tt.input, got, tt.wantD)
			}
		})
	}
}
