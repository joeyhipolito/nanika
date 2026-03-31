package browser

import (
	"testing"
)

func TestExtractItems_BasicLinks(t *testing.T) {
	html := `<html><body>
		<article>
			<a href="https://example.com/article-one">How Go handles errors</a>
			<p>A deep dive into Go error handling patterns and best practices.</p>
		</article>
		<article>
			<a href="https://other.com/post">Building CLI tools in Go</a>
			<p>A guide to building robust command-line interfaces.</p>
		</article>
	</body></html>`

	items := ExtractItems(html, "", "test")
	if len(items) < 2 {
		t.Fatalf("expected at least 2 items, got %d", len(items))
	}
}

func TestExtractItems_FiltersNavLinks(t *testing.T) {
	html := `<html><body>
		<nav>
			<a href="https://example.com/home">Home</a>
			<a href="https://example.com/about">About</a>
		</nav>
		<article>
			<a href="https://external.com/real-article">Real article title here</a>
		</article>
	</body></html>`

	items := ExtractItems(html, "", "test")
	for _, item := range items {
		if item.URL == "https://example.com/home" || item.URL == "https://example.com/about" {
			t.Errorf("nav link %s should have been filtered", item.URL)
		}
	}
}

func TestExtractItems_FiltersJavascriptAndFragment(t *testing.T) {
	html := `<html><body>
		<article>
			<a href="javascript:void(0)">Click me</a>
			<a href="#section">Jump</a>
			<a href="https://real.com/article">Real article title here</a>
		</article>
	</body></html>`

	items := ExtractItems(html, "", "test")
	for _, item := range items {
		if item.URL == "javascript:void(0)" || item.URL == "#section" {
			t.Errorf("invalid href %s should have been filtered", item.URL)
		}
	}
}

func TestExtractItems_DeduplicatesByURL(t *testing.T) {
	html := `<html><body>
		<article>
			<a href="https://example.com/article">Same article link title</a>
		</article>
		<article>
			<a href="https://example.com/article">Same article link title</a>
		</article>
	</body></html>`

	items := ExtractItems(html, "", "test")
	if len(items) != 1 {
		t.Errorf("expected 1 deduplicated item, got %d", len(items))
	}
}

func TestExtractItems_HostFilter(t *testing.T) {
	html := `<html><body>
		<article>
			<a href="https://google.com/search?q=test">Google Search</a>
			<a href="https://nytimes.com/article">External article title here</a>
		</article>
	</body></html>`

	items := ExtractItems(html, "google.com", "test")
	for _, item := range items {
		if item.URL == "https://google.com/search?q=test" {
			t.Error("google.com link should have been filtered by host filter")
		}
	}
	found := false
	for _, item := range items {
		if item.URL == "https://nytimes.com/article" {
			found = true
		}
	}
	if !found {
		t.Error("external article link should not have been filtered")
	}
}

func TestExtractItems_ShortLinkTextFallsBackToSurroundingText(t *testing.T) {
	// Simulates a social feed where the permalink <a> contains only a timestamp.
	html := `<html><body>
		<article>
			<div>This is the post content that should become the title</div>
			<a href="https://x.com/user/status/123456789">2h</a>
		</article>
	</body></html>`

	items := ExtractItems(html, "", "test")
	if len(items) == 0 {
		t.Fatal("expected items, got none")
	}
	// The title should not be "2h" — it should come from the surrounding article text.
	for _, item := range items {
		if item.URL == "https://x.com/user/status/123456789" {
			if item.Title == "2h" {
				t.Errorf("expected surrounding text as title, got short link text %q", item.Title)
			}
			if len(item.Title) < 5 {
				t.Errorf("expected meaningful title, got %q", item.Title)
			}
		}
	}
}

func TestExtractItems_SkipsRelativeURLs(t *testing.T) {
	html := `<html><body>
		<article>
			<a href="/relative/path">Relative link text here</a>
			<a href="https://absolute.com/path">Absolute link title</a>
		</article>
	</body></html>`

	items := ExtractItems(html, "", "test")
	for _, item := range items {
		if item.URL == "/relative/path" {
			t.Error("relative URL should have been filtered")
		}
	}
}

func TestExtractItems_SnippetTruncatedAt300(t *testing.T) {
	long := ""
	for i := 0; i < 400; i++ {
		long += "x"
	}
	html := `<html><body><article><p>` + long + `</p><a href="https://example.com/a">Long snippet article title</a></article></body></html>`

	items := ExtractItems(html, "", "test")
	for _, item := range items {
		if len(item.Snippet) > 303 { // 300 + "..."
			t.Errorf("snippet too long: %d chars", len(item.Snippet))
		}
	}
}

func TestExtractItems_EmptyHTML(t *testing.T) {
	items := ExtractItems("", "", "test")
	if items != nil {
		t.Errorf("expected nil for empty HTML, got %d items", len(items))
	}
}

func TestExtractItems_SourceNamePropagated(t *testing.T) {
	html := `<html><body>
		<article>
			<a href="https://example.com/post">Article title that is long enough</a>
		</article>
	</body></html>`

	items := ExtractItems(html, "", "MySource")
	if len(items) == 0 {
		t.Fatal("expected items")
	}
	if items[0].Source != "MySource" {
		t.Errorf("expected source 'MySource', got %q", items[0].Source)
	}
}
