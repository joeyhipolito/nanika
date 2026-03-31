// Package browser provides a CDP client and shared HTML extraction utilities
// for browser-based gatherers.
package browser

import (
	"net/url"
	"strings"

	"github.com/PuerkitoBio/goquery"
	"github.com/go-shiori/go-readability"
)

// ExtractedItem is a single link and its surrounding context from raw HTML.
type ExtractedItem struct {
	Title   string
	URL     string
	Snippet string
	Source  string
}

// navTags are element names that indicate navigational context.
// Links inside these elements are treated as chrome/nav and filtered out.
var navTags = []string{"nav", "header", "footer"}

// ExtractItems parses rawHTML and returns deduplicated content links.
//
// filterHost skips links whose host contains the given string. Pass "google.com"
// for Google Discover (where all interesting links are external) and "" for social
// feeds (where post URLs live on the same host as the page).
func ExtractItems(rawHTML, filterHost, sourceName string) []ExtractedItem {
	doc, err := goquery.NewDocumentFromReader(strings.NewReader(rawHTML))
	if err != nil {
		return nil
	}

	seen := make(map[string]bool)
	var items []ExtractedItem

	doc.Find("a[href]").Each(func(_ int, a *goquery.Selection) {
		href, _ := a.Attr("href")
		href = strings.TrimSpace(href)

		// Skip non-navigable or fragment-only hrefs.
		if href == "" ||
			strings.HasPrefix(href, "#") ||
			strings.HasPrefix(href, "javascript:") ||
			strings.HasPrefix(href, "mailto:") ||
			strings.HasPrefix(href, "tel:") {
			return
		}

		// Skip links inside navigational elements.
		for _, tag := range navTags {
			if a.Closest(tag).Length() > 0 {
				return
			}
		}

		// Require an absolute URL with a host.
		parsed, parseErr := url.Parse(href)
		if parseErr != nil || parsed.Host == "" {
			return
		}

		// Apply the optional host filter (e.g. drop google.com links on Google pages).
		if filterHost != "" && strings.Contains(parsed.Host, filterHost) {
			return
		}

		// Deduplicate by lower-cased host+path.
		key := strings.ToLower(parsed.Host + parsed.Path)
		if seen[key] {
			return
		}

		// Derive title from link text; fall back to surrounding block text for
		// social-feed permalink links that only contain a timestamp ("2h", "Mar 24").
		title := strings.Join(strings.Fields(a.Text()), " ")
		snippet := extractSnippet(a)
		if len(title) < 5 {
			title = truncateStr(snippet, 100)
			snippet = "" // title == snippet would be redundant
		}
		if len(title) < 5 {
			return
		}

		seen[key] = true

		if len(snippet) > 300 {
			snippet = snippet[:300] + "..."
		}

		items = append(items, ExtractedItem{
			Title:   title,
			URL:     href,
			Snippet: snippet,
			Source:  sourceName,
		})
	})

	return items
}

// extractSnippet returns text from the nearest meaningful block ancestor of a.
// It prefers article > p > li > div to avoid enormous blobs of page text.
func extractSnippet(a *goquery.Selection) string {
	linkText := strings.Join(strings.Fields(a.Text()), " ")
	for _, tag := range []string{"article", "p", "li", "div"} {
		block := a.Closest(tag)
		if block.Length() == 0 {
			continue
		}
		text := strings.Join(strings.Fields(block.Text()), " ")
		// Only useful when the block contains more than the link text itself.
		if len(text) > len(linkText)+10 {
			return text
		}
	}
	return ""
}

// truncateStr returns s truncated to maxLen rune positions (byte-safe for ASCII).
func truncateStr(s string, maxLen int) string {
	if len(s) <= maxLen {
		return s
	}
	return s[:maxLen]
}

// ReadabilityExtract uses go-readability to extract the main article from rawHTML.
// It is intended for single-article pages; feed pages should use ExtractItems instead.
// Returns false when extraction fails or the article body is shorter than 200 characters.
func ReadabilityExtract(rawHTML, rawURL, sourceName string) (ExtractedItem, bool) {
	parsed, err := url.Parse(rawURL)
	if err != nil {
		return ExtractedItem{}, false
	}

	article, err := readability.FromReader(strings.NewReader(rawHTML), parsed)
	if err != nil || article.Title == "" || article.Length < 200 {
		return ExtractedItem{}, false
	}

	snippet := article.Excerpt
	if snippet == "" {
		snippet = truncateStr(strings.Join(strings.Fields(article.TextContent), " "), 300)
	}

	return ExtractedItem{
		Title:   article.Title,
		URL:     rawURL,
		Snippet: snippet,
		Source:  sourceName,
	}, true
}
