package vault

import (
	"testing"
)

func TestParseNote_WithFrontmatter(t *testing.T) {
	content := "---\ntitle: Test Note\ndate: 2026-02-07\ntags:\n  - daily\n  - work\n---\n\n# Hello\n\nSome text with [[link1]] and [[link2|alias]].\n"

	note := ParseNote(content)

	if note.Frontmatter["title"] != "Test Note" {
		t.Errorf("expected title 'Test Note', got %v", note.Frontmatter["title"])
	}
	if note.Frontmatter["date"] != "2026-02-07" {
		t.Errorf("expected date '2026-02-07', got %v", note.Frontmatter["date"])
	}

	tags, ok := note.Frontmatter["tags"].([]string)
	if !ok {
		t.Fatalf("expected tags to be []string, got %T", note.Frontmatter["tags"])
	}
	if len(tags) != 2 || tags[0] != "daily" || tags[1] != "work" {
		t.Errorf("expected tags [daily, work], got %v", tags)
	}

	if len(note.Headings) != 1 || note.Headings[0].Text != "Hello" || note.Headings[0].Level != 1 {
		t.Errorf("expected one H1 'Hello', got %v", note.Headings)
	}

	if len(note.Wikilinks) != 2 || note.Wikilinks[0] != "link1" || note.Wikilinks[1] != "link2" {
		t.Errorf("expected wikilinks [link1, link2], got %v", note.Wikilinks)
	}
}

func TestParseNote_NoFrontmatter(t *testing.T) {
	content := "# Just a heading\n\nSome body text.\n"

	note := ParseNote(content)

	if len(note.Frontmatter) != 0 {
		t.Errorf("expected empty frontmatter, got %v", note.Frontmatter)
	}
	if note.Body != content {
		t.Errorf("expected body to be full content, got %q", note.Body)
	}
	if len(note.Headings) != 1 || note.Headings[0].Text != "Just a heading" {
		t.Errorf("expected heading 'Just a heading', got %v", note.Headings)
	}
}

func TestParseNote_InlineList(t *testing.T) {
	content := "---\ntags: [foo, bar, baz]\n---\n\nBody.\n"

	note := ParseNote(content)

	tags, ok := note.Frontmatter["tags"].([]string)
	if !ok {
		t.Fatalf("expected tags to be []string, got %T", note.Frontmatter["tags"])
	}
	if len(tags) != 3 || tags[0] != "foo" || tags[1] != "bar" || tags[2] != "baz" {
		t.Errorf("expected [foo, bar, baz], got %v", tags)
	}
}

func TestParseNote_QuotedValues(t *testing.T) {
	content := "---\ntitle: \"Hello: World\"\nauthor: 'Jane Doe'\n---\n\nBody.\n"

	note := ParseNote(content)

	if note.Frontmatter["title"] != "Hello: World" {
		t.Errorf("expected title 'Hello: World', got %v", note.Frontmatter["title"])
	}
	if note.Frontmatter["author"] != "Jane Doe" {
		t.Errorf("expected author 'Jane Doe', got %v", note.Frontmatter["author"])
	}
}

func TestParseNote_MultipleHeadings(t *testing.T) {
	content := "# H1\n## H2\n### H3\ntext\n## Another H2\n"

	note := ParseNote(content)

	if len(note.Headings) != 4 {
		t.Fatalf("expected 4 headings, got %d", len(note.Headings))
	}
	if note.Headings[0].Level != 1 || note.Headings[0].Text != "H1" {
		t.Errorf("heading 0: expected H1 level 1, got %v", note.Headings[0])
	}
	if note.Headings[1].Level != 2 || note.Headings[1].Text != "H2" {
		t.Errorf("heading 1: expected H2 level 2, got %v", note.Headings[1])
	}
	if note.Headings[2].Level != 3 || note.Headings[2].Text != "H3" {
		t.Errorf("heading 2: expected H3 level 3, got %v", note.Headings[2])
	}
	if note.Headings[3].Level != 2 || note.Headings[3].Text != "Another H2" {
		t.Errorf("heading 3: expected Another H2 level 2, got %v", note.Headings[3])
	}
}

func TestParseNote_DuplicateWikilinks(t *testing.T) {
	content := "See [[note1]] and then [[note1]] again, plus [[note2]].\n"

	note := ParseNote(content)

	if len(note.Wikilinks) != 2 {
		t.Errorf("expected 2 unique wikilinks, got %v", note.Wikilinks)
	}
}

func TestParseNote_EmptyContent(t *testing.T) {
	note := ParseNote("")

	if len(note.Frontmatter) != 0 {
		t.Errorf("expected empty frontmatter, got %v", note.Frontmatter)
	}
	if note.Body != "" {
		t.Errorf("expected empty body, got %q", note.Body)
	}
}

func TestFormatFrontmatter(t *testing.T) {
	fm := map[string]any{
		"title": "Test",
	}

	result := FormatFrontmatter(fm)

	if result == "" {
		t.Error("expected non-empty frontmatter")
	}
	if result[:4] != "---\n" {
		t.Errorf("expected to start with ---, got %q", result[:4])
	}
	if result[len(result)-4:] != "---\n" {
		t.Errorf("expected to end with ---, got %q", result[len(result)-4:])
	}
}

func TestFormatFrontmatter_Empty(t *testing.T) {
	result := FormatFrontmatter(map[string]any{})
	if result != "" {
		t.Errorf("expected empty string for empty map, got %q", result)
	}
}
