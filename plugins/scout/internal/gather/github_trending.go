package gather

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"
)

// ensure interface compliance at compile time
var _ Gatherer = (*GitHubTrendingGatherer)(nil)

// GitHubTrendingGatherer scrapes the GitHub trending page for popular repos.
// Uses the unofficial trending page (no API key required).
type GitHubTrendingGatherer struct {
	Client    *http.Client
	Languages []string // optional language filters; empty means all languages
}

// NewGitHubTrendingGatherer creates a new GitHub trending gatherer.
// languages may be empty (fetches global trending) or specify languages like "go", "python".
func NewGitHubTrendingGatherer(languages []string) *GitHubTrendingGatherer {
	return &GitHubTrendingGatherer{
		Client:    newHTTPClient(),
		Languages: languages,
	}
}

// Name returns the canonical source identifier.
func (g *GitHubTrendingGatherer) Name() string { return "github-trending" }

// Gather fetches GitHub trending repos, optionally filtered by configured languages.
// searchTerms are applied as a post-fetch filter on title/content.
func (g *GitHubTrendingGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	langs := g.Languages
	if len(langs) == 0 {
		langs = []string{""} // fetch global trending
	}

	seen := make(map[string]bool)
	var all []IntelItem
	var lastErr error

	for _, lang := range langs {
		items, err := g.fetchTrending(ctx, lang)
		if err != nil {
			lastErr = err
			fmt.Fprintf(os.Stderr, "    Warning: github-trending/%s: %v\n", lang, err)
			continue
		}
		all = appendUnique(seen, all, items)
	}

	if len(all) == 0 && lastErr != nil {
		return nil, fmt.Errorf("github-trending: all fetches failed, last: %w", lastErr)
	}

	if len(searchTerms) > 0 {
		all = filterByTerms(all, searchTerms)
	}
	return all, nil
}

// fetchTrending fetches the trending page for a given language (or all if empty).
func (g *GitHubTrendingGatherer) fetchTrending(ctx context.Context, language string) ([]IntelItem, error) {
	trendingURL := "https://github.com/trending"
	if language != "" {
		trendingURL += "/" + strings.ToLower(strings.ReplaceAll(language, " ", "-"))
	}
	trendingURL += "?since=daily"

	req, err := http.NewRequestWithContext(ctx, "GET", trendingURL, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}
	req.Header.Set("User-Agent", userAgent)
	req.Header.Set("Accept", "text/html,application/xhtml+xml")

	resp, err := g.Client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch trending page: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusTooManyRequests {
		return nil, fmt.Errorf("rate limited (429). Wait and retry")
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d", resp.StatusCode)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response: %w", err)
	}

	return ParseGitHubTrendingHTML(body, language)
}

// ParseGitHubTrendingHTML parses the GitHub trending HTML page and returns IntelItems.
// Exported for testing.
func ParseGitHubTrendingHTML(body []byte, language string) ([]IntelItem, error) {
	html := string(body)

	// Each trending repo is wrapped in <article class="Box-row">
	parts := strings.Split(html, `<article class="Box-row">`)

	var items []IntelItem
	for _, part := range parts[1:] { // skip content before first article
		// Trim to the closing </article> to avoid bleed-over between entries
		if end := strings.Index(part, "</article>"); end > 0 {
			part = part[:end]
		}
		item, ok := parseTrendingArticle(part, language)
		if !ok {
			continue
		}
		items = append(items, item)
	}
	return items, nil
}

// parseTrendingArticle extracts a single IntelItem from one article's HTML.
func parseTrendingArticle(article, language string) (IntelItem, bool) {
	// Repo link: <h2 ...><a href="/owner/repo">
	// The href always appears inside the heading before the description.
	repoPath := trendingBetween(article, `href="/`, `"`)
	if repoPath == "" || strings.Contains(repoPath, " ") {
		return IntelItem{}, false
	}
	// repoPath must be "owner/repo" (exactly one slash)
	slashIdx := strings.Index(repoPath, "/")
	if slashIdx < 0 || strings.Count(repoPath, "/") != 1 {
		return IntelItem{}, false
	}
	owner := repoPath[:slashIdx]
	repo := repoPath[slashIdx+1:]
	if owner == "" || repo == "" {
		return IntelItem{}, false
	}

	// Description: <p class="col-9 color-fg-muted my-1 pr-4">text</p>
	desc := trendingBetween(article, `col-9 color-fg-muted my-1 pr-4">`, `</p>`)
	desc = strings.TrimSpace(cleanText(desc))

	// Language: <span itemprop="programmingLanguage">Go</span>
	lang := trendingBetween(article, `itemprop="programmingLanguage">`, `</span>`)
	lang = strings.TrimSpace(lang)
	if lang == "" && language != "" {
		lang = language
	}

	// Total stars: number before first "/stargazers" anchor text
	totalStars := parseTrendingStars(article, "/stargazers")

	// Stars today: text in the float-sm-right span (e.g. "456 stars today")
	starsToday := 0
	todayBlock := trendingBetween(article, `float-sm-right">`, `</span>`)
	if todayBlock != "" {
		todayClean := cleanText(todayBlock)
		fields := strings.Fields(todayClean)
		if len(fields) > 0 {
			n, err := strconv.Atoi(strings.ReplaceAll(fields[0], ",", ""))
			if err == nil {
				starsToday = n
			}
		}
	}

	fullName := owner + "/" + repo
	sourceURL := "https://github.com/" + fullName

	tags := []string{"github-trending"}
	if lang != "" {
		tags = append(tags, strings.ToLower(lang))
	}

	content := desc
	if len(content) > 500 {
		content = content[:500] + "..."
	}

	return IntelItem{
		ID:         generateID(fullName),
		Title:      fullName,
		Content:    content,
		SourceURL:  sourceURL,
		Author:     owner,
		Timestamp:  time.Now().UTC(),
		Tags:       tags,
		Engagement: totalStars + starsToday,
	}, true
}

// parseTrendingStars finds the first numeric text after a href containing hrefFragment.
func parseTrendingStars(html, hrefFragment string) int {
	idx := strings.Index(html, hrefFragment)
	if idx < 0 {
		return 0
	}
	// Advance past the closing > of the anchor tag
	rest := html[idx:]
	closeIdx := strings.Index(rest, ">")
	if closeIdx < 0 {
		return 0
	}
	// Grab text up to </a>
	text := trendingBetween(rest[closeIdx:], ">", "</a>")
	text = strings.TrimSpace(cleanText(text))
	fields := strings.Fields(text)
	if len(fields) == 0 {
		return 0
	}
	n, err := strconv.Atoi(strings.ReplaceAll(fields[0], ",", ""))
	if err != nil {
		return 0
	}
	return n
}

// trendingBetween returns the substring between start and end markers, or "" if not found.
func trendingBetween(s, start, end string) string {
	i := strings.Index(s, start)
	if i < 0 {
		return ""
	}
	s = s[i+len(start):]
	j := strings.Index(s, end)
	if j < 0 {
		return ""
	}
	return s[:j]
}
