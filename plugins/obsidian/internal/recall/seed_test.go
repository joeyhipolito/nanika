package recall

import "testing"

// TestTagMatch: ByTag returns exactly the documents whose Tags slice contains
// the requested tag, and no others.
func TestTagMatch(t *testing.T) {
	docs := []Document{
		{Path: "tagged.md", Tags: []string{"go", "programming"}},
		{Path: "other.md", Tags: []string{"cooking"}},
		{Path: "multi.md", Tags: []string{"go", "testing"}},
		{Path: "none.md", Tags: nil},
	}
	sf := NewSeedFilter(docs)
	got := sf.ByTag("go")
	if len(got) != 2 {
		t.Fatalf("ByTag(\"go\"): want 2 documents, got %d", len(got))
	}
	for _, d := range got {
		found := false
		for _, tag := range d.Tags {
			if tag == "go" {
				found = true
				break
			}
		}
		if !found {
			t.Errorf("returned document %q is missing tag \"go\"", d.Path)
		}
	}
	// Verify none.md and other.md are absent.
	for _, d := range got {
		if d.Path == "other.md" || d.Path == "none.md" {
			t.Errorf("ByTag(\"go\") must not return %q", d.Path)
		}
	}
}

// TestTitlePrefix: ByTitlePrefix returns exactly the documents whose Title
// starts with the given prefix, case-sensitive.
func TestTitlePrefix(t *testing.T) {
	docs := []Document{
		{Path: "daily1.md", Title: "2026-04-21 Daily Note"},
		{Path: "daily2.md", Title: "2026-04-20 Daily Note"},
		{Path: "meeting.md", Title: "Meeting Notes April"},
		{Path: "empty.md", Title: ""},
	}
	sf := NewSeedFilter(docs)

	got := sf.ByTitlePrefix("2026-04")
	if len(got) != 2 {
		t.Fatalf("ByTitlePrefix(\"2026-04\"): want 2, got %d", len(got))
	}
	for _, d := range got {
		if d.Path == "meeting.md" || d.Path == "empty.md" {
			t.Errorf("ByTitlePrefix returned unexpected document %q", d.Path)
		}
	}

	// Empty prefix matches all documents.
	all := sf.ByTitlePrefix("")
	if len(all) != len(docs) {
		t.Errorf("ByTitlePrefix(\"\") want %d, got %d", len(docs), len(all))
	}
}
