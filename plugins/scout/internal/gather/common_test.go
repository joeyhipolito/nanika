package gather

import (
	"testing"
)

func TestNormalizeURL_StripsSchemeAndWWW(t *testing.T) {
	tests := []struct {
		input    string
		expected string
	}{
		{"https://www.example.com/article", "example.com/article"},
		{"http://www.example.com/article", "example.com/article"},
		{"https://example.com/article", "example.com/article"},
	}

	for _, tc := range tests {
		got := normalizeURL(tc.input)
		if got != tc.expected {
			t.Errorf("normalizeURL(%q) = %q, want %q", tc.input, got, tc.expected)
		}
	}
}

func TestNormalizeURL_StripsTrailingSlash(t *testing.T) {
	a := normalizeURL("https://example.com/post/")
	b := normalizeURL("https://example.com/post")
	if a != b {
		t.Errorf("trailing slash not normalized: %q vs %q", a, b)
	}
}

func TestNormalizeURL_StripsTrackingParams(t *testing.T) {
	tests := []struct {
		input    string
		expected string
	}{
		{"https://example.com/post?utm_source=twitter&utm_medium=social", "example.com/post"},
		{"https://example.com/post?ref=homepage", "example.com/post"},
		{"https://example.com/post?source=newsletter", "example.com/post"},
		{"https://example.com/post?utm_campaign=launch&id=42", "example.com/post?id=42"},
	}

	for _, tc := range tests {
		got := normalizeURL(tc.input)
		if got != tc.expected {
			t.Errorf("normalizeURL(%q) = %q, want %q", tc.input, got, tc.expected)
		}
	}
}

func TestNormalizeURL_PreservesNonTrackingParams(t *testing.T) {
	got := normalizeURL("https://example.com/search?q=golang&page=2")
	if got != "example.com/search?page=2&q=golang" {
		t.Errorf("normalizeURL should preserve non-tracking params, got %q", got)
	}
}

func TestNormalizeURL_InvalidURL(t *testing.T) {
	// Non-URL strings should be returned as-is
	input := "not-a-url"
	got := normalizeURL(input)
	if got != input {
		t.Errorf("expected invalid URL returned as-is, got %q", got)
	}
}

func TestNormalizeURL_CaseFolding(t *testing.T) {
	a := normalizeURL("https://Example.COM/Article")
	b := normalizeURL("https://example.com/Article")
	if a != b {
		t.Errorf("expected case-insensitive host normalization: %q vs %q", a, b)
	}
}

func TestStripTags(t *testing.T) {
	tests := []struct {
		input    string
		expected string
	}{
		{"<p>Hello</p>", " Hello "},
		{"No tags here", "No tags here"},
		{"<b>Bold</b> and <i>italic</i>", " Bold  and  italic "},
		{"", ""},
	}

	for _, tc := range tests {
		got := stripTags(tc.input)
		if got != tc.expected {
			t.Errorf("stripTags(%q) = %q, want %q", tc.input, got, tc.expected)
		}
	}
}

func TestCleanText(t *testing.T) {
	tests := []struct {
		input    string
		expected string
	}{
		{"Hello &amp; world", "Hello & world"},
		// &lt;tags&gt; becomes <tags> after entity decode, then gets stripped
		{"<p>HTML &lt;tags&gt;</p>", "HTML"},
		{"  extra   spaces  ", "extra spaces"},
		{"&nbsp;non-breaking&nbsp;space", "non-breaking space"},
	}

	for _, tc := range tests {
		got := cleanText(tc.input)
		if got != tc.expected {
			t.Errorf("cleanText(%q) = %q, want %q", tc.input, got, tc.expected)
		}
	}
}

func TestFilterByTerms(t *testing.T) {
	items := []IntelItem{
		{Title: "Go programming tips", Content: "Learn Go"},
		{Title: "Python web framework", Content: "Django guide"},
		{Title: "Rust memory safety", Content: "Borrow checker explained"},
	}

	filtered := filterByTerms(items, []string{"Go", "Python"})
	if len(filtered) != 2 {
		t.Fatalf("expected 2 items matching 'Go' or 'Python', got %d", len(filtered))
	}
}

func TestFilterByTerms_CaseInsensitive(t *testing.T) {
	items := []IntelItem{
		{Title: "GOLANG tips"},
	}
	filtered := filterByTerms(items, []string{"golang"})
	if len(filtered) != 1 {
		t.Errorf("expected case-insensitive match, got %d items", len(filtered))
	}
}

func TestFilterByTerms_MatchesAuthor(t *testing.T) {
	items := []IntelItem{
		{Title: "Unrelated", Author: "gopher_dev"},
	}
	filtered := filterByTerms(items, []string{"gopher"})
	if len(filtered) != 1 {
		t.Errorf("expected author match, got %d items", len(filtered))
	}
}

func TestGenerateID_Deterministic(t *testing.T) {
	a := generateID("https://example.com/article")
	b := generateID("https://example.com/article")
	if a != b {
		t.Errorf("expected deterministic ID, got %q and %q", a, b)
	}
}

func TestGenerateID_DifferentInputsDifferentIDs(t *testing.T) {
	a := generateID("https://example.com/a")
	b := generateID("https://example.com/b")
	if a == b {
		t.Errorf("expected different IDs for different inputs, both got %q", a)
	}
}
