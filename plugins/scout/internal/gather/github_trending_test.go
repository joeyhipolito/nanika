package gather

import (
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// minimalTrendingHTML builds a minimal but realistic GitHub trending HTML page
// containing the given articles.
func minimalTrendingHTML(articles ...string) string {
	return `<!DOCTYPE html><html><body><div class="Box">` +
		strings.Join(articles, "\n") +
		`</div></body></html>`
}

// trendingArticle builds a single trending article HTML fragment.
func trendingArticle(owner, repo, desc, lang string, stars, starsToday int) string {
	langSpan := ""
	if lang != "" {
		langSpan = fmt.Sprintf(`<span itemprop="programmingLanguage">%s</span>`, lang)
	}
	return fmt.Sprintf(`<article class="Box-row">
  <h2 class="h3 lh-condensed">
    <a href="/%s/%s">
      <span class="text-normal">%s /</span>
      %s
    </a>
  </h2>
  <p class="col-9 color-fg-muted my-1 pr-4">
    %s
  </p>
  <div class="f6 color-fg-muted mt-2">
    %s
    <a class="Link--muted d-inline-block mr-3" href="/%s/%s/stargazers">
      <svg aria-label="star"></svg>
      %s
    </a>
    <span class="d-inline-block float-sm-right">
      <a href="/%s/%s/stargazers">
        <svg aria-label="star"></svg>
        %d stars today
      </a>
    </span>
  </div>
</article>`,
		owner, repo, owner, repo,
		desc,
		langSpan,
		owner, repo, formatStars(stars),
		owner, repo, starsToday,
	)
}

func formatStars(n int) string {
	if n >= 1000 {
		return fmt.Sprintf("%d,%03d", n/1000, n%1000)
	}
	return fmt.Sprintf("%d", n)
}

func TestParseGitHubTrendingHTML_HappyPath(t *testing.T) {
	html := minimalTrendingHTML(
		trendingArticle("golang", "go", "The Go programming language", "Go", 120000, 250),
		trendingArticle("microsoft", "vscode", "Visual Studio Code", "TypeScript", 165000, 180),
	)

	items, err := ParseGitHubTrendingHTML([]byte(html), "")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 2 {
		t.Fatalf("expected 2 items, got %d", len(items))
	}

	// First item
	if items[0].Title != "golang/go" {
		t.Errorf("expected title golang/go, got %s", items[0].Title)
	}
	if items[0].SourceURL != "https://github.com/golang/go" {
		t.Errorf("unexpected source URL: %s", items[0].SourceURL)
	}
	if items[0].Author != "golang" {
		t.Errorf("expected author golang, got %s", items[0].Author)
	}
	if items[0].Content == "" {
		t.Error("expected non-empty content")
	}

	// Language tag
	hasGoTag := false
	for _, tag := range items[0].Tags {
		if tag == "go" {
			hasGoTag = true
		}
	}
	if !hasGoTag {
		t.Errorf("expected 'go' language tag, got %v", items[0].Tags)
	}

	hasTrendingTag := false
	for _, tag := range items[0].Tags {
		if tag == "github-trending" {
			hasTrendingTag = true
		}
	}
	if !hasTrendingTag {
		t.Error("expected 'github-trending' tag")
	}
}

func TestParseGitHubTrendingHTML_Engagement(t *testing.T) {
	html := minimalTrendingHTML(
		trendingArticle("owner", "repo", "desc", "Go", 5000, 123),
	)

	items, err := ParseGitHubTrendingHTML([]byte(html), "go")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 1 {
		t.Fatalf("expected 1 item, got %d", len(items))
	}

	// Engagement = totalStars + starsToday
	if items[0].Engagement != 5123 {
		t.Errorf("expected engagement 5123, got %d", items[0].Engagement)
	}
}

func TestParseGitHubTrendingHTML_EmptyBody(t *testing.T) {
	items, err := ParseGitHubTrendingHTML([]byte(`<html></html>`), "")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 0 {
		t.Errorf("expected 0 items, got %d", len(items))
	}
}

func TestParseGitHubTrendingHTML_NoDescription(t *testing.T) {
	html := minimalTrendingHTML(`<article class="Box-row">
  <h2 class="h3 lh-condensed">
    <a href="/foo/bar">
      <span class="text-normal">foo /</span>
      bar
    </a>
  </h2>
  <div class="f6 color-fg-muted mt-2">
    <a href="/foo/bar/stargazers"><svg></svg> 500</a>
    <span class="d-inline-block float-sm-right">
      <a href="/foo/bar/stargazers"><svg></svg> 10 stars today</a>
    </span>
  </div>
</article>`)

	items, err := ParseGitHubTrendingHTML([]byte(html), "")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 1 {
		t.Fatalf("expected 1 item, got %d", len(items))
	}
	if items[0].Title != "foo/bar" {
		t.Errorf("expected title foo/bar, got %s", items[0].Title)
	}
	if items[0].Content != "" {
		t.Errorf("expected empty content for no description, got %q", items[0].Content)
	}
}

func TestParseGitHubTrendingHTML_LanguageFallback(t *testing.T) {
	// Article has no language span; should fall back to the provided language param
	html := minimalTrendingHTML(`<article class="Box-row">
  <h2 class="h3 lh-condensed">
    <a href="/owner/repo">
      <span class="text-normal">owner /</span>
      repo
    </a>
  </h2>
  <p class="col-9 color-fg-muted my-1 pr-4">desc</p>
  <div class="f6 color-fg-muted mt-2">
    <a href="/owner/repo/stargazers"><svg></svg> 100</a>
  </div>
</article>`)

	items, err := ParseGitHubTrendingHTML([]byte(html), "rust")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 1 {
		t.Fatalf("expected 1 item, got %d", len(items))
	}

	hasRust := false
	for _, tag := range items[0].Tags {
		if tag == "rust" {
			hasRust = true
		}
	}
	if !hasRust {
		t.Errorf("expected language fallback tag 'rust', got %v", items[0].Tags)
	}
}

func TestParseGitHubTrendingHTML_ContentTruncation(t *testing.T) {
	longDesc := strings.Repeat("a", 600)
	html := minimalTrendingHTML(trendingArticle("owner", "repo", longDesc, "Go", 100, 5))

	items, err := ParseGitHubTrendingHTML([]byte(html), "")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 1 {
		t.Fatalf("expected 1 item, got %d", len(items))
	}
	if len(items[0].Content) > 503 { // 500 + "..."
		t.Errorf("expected content truncated, got len %d", len(items[0].Content))
	}
	if !strings.HasSuffix(items[0].Content, "...") {
		t.Errorf("expected content to end with '...', got %q", items[0].Content)
	}
}

func TestGitHubTrendingGatherer_Gather_HappyPath(t *testing.T) {
	html := minimalTrendingHTML(
		trendingArticle("golang", "go", "The Go programming language", "Go", 120000, 250),
	)

	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		fmt.Fprint(w, html)
	}))
	defer ts.Close()

	g := NewGitHubTrendingGatherer(nil)
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) == 0 {
		t.Fatal("expected items, got none")
	}
	if items[0].Title != "golang/go" {
		t.Errorf("unexpected title: %s", items[0].Title)
	}
}

func TestGitHubTrendingGatherer_Gather_MultipleLanguages(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// Serve different repos depending on URL path
		if strings.Contains(r.URL.Path, "/go") {
			fmt.Fprint(w, minimalTrendingHTML(
				trendingArticle("golang", "go", "The Go language", "Go", 120000, 250),
			))
		} else {
			fmt.Fprint(w, minimalTrendingHTML(
				trendingArticle("python", "cpython", "The Python language", "Python", 60000, 100),
			))
		}
	}))
	defer ts.Close()

	g := NewGitHubTrendingGatherer([]string{"go", "python"})
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	items, err := g.Gather(context.Background(), nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(items) != 2 {
		t.Errorf("expected 2 items (one per language), got %d", len(items))
	}
}

func TestGitHubTrendingGatherer_Gather_SearchTermFilter(t *testing.T) {
	html := minimalTrendingHTML(
		trendingArticle("golang", "go", "The Go programming language", "Go", 120000, 250),
		trendingArticle("microsoft", "vscode", "Code editor", "TypeScript", 165000, 180),
	)

	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		fmt.Fprint(w, html)
	}))
	defer ts.Close()

	g := NewGitHubTrendingGatherer(nil)
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	// Filter to only items matching "Go"
	items, err := g.Gather(context.Background(), []string{"golang"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	for _, item := range items {
		if !strings.Contains(strings.ToLower(item.Title+item.Content+item.Author), "golang") {
			t.Errorf("item %q does not match search term 'golang'", item.Title)
		}
	}
}

func TestGitHubTrendingGatherer_Gather_HTTPError(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
	}))
	defer ts.Close()

	g := NewGitHubTrendingGatherer(nil)
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	_, err := g.Gather(context.Background(), nil)
	if err == nil {
		t.Fatal("expected error for 500 response")
	}
}

func TestGitHubTrendingGatherer_Gather_RateLimit(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusTooManyRequests)
	}))
	defer ts.Close()

	g := NewGitHubTrendingGatherer(nil)
	g.Client.Transport = &rewriteTransport{URL: ts.URL}

	_, err := g.Gather(context.Background(), nil)
	if err == nil {
		t.Fatal("expected error for 429 response")
	}
}

func TestGitHubTrendingGatherer_Name(t *testing.T) {
	g := NewGitHubTrendingGatherer(nil)
	if g.Name() != "github-trending" {
		t.Errorf("expected name 'github-trending', got %s", g.Name())
	}
}

func TestTrendingBetween(t *testing.T) {
	tests := []struct {
		s, start, end string
		want          string
	}{
		{"hello world", "hello ", "d", "worl"},
		{"<tag>content</tag>", "<tag>", "</tag>", "content"},
		{"no match", "x", "y", ""},
		{"start but no finish", "start", "end", ""},
	}
	for _, tc := range tests {
		got := trendingBetween(tc.s, tc.start, tc.end)
		if got != tc.want {
			t.Errorf("trendingBetween(%q, %q, %q) = %q, want %q", tc.s, tc.start, tc.end, got, tc.want)
		}
	}
}

func TestParseTrendingStars(t *testing.T) {
	html := `<a href="/owner/repo/stargazers"><svg></svg> 1,234 </a>`
	n := parseTrendingStars(html, "/stargazers")
	if n != 1234 {
		t.Errorf("expected 1234, got %d", n)
	}
}

func TestParseTrendingStars_NoMatch(t *testing.T) {
	n := parseTrendingStars("no stargazers link here", "/stargazers")
	if n != 0 {
		t.Errorf("expected 0, got %d", n)
	}
}
