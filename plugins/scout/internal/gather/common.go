package gather

import (
	"crypto/sha256"
	"fmt"
	"net/url"
	"strings"
)

// stripTags removes all HTML tags from a string.
func stripTags(html string) string {
	var result strings.Builder
	inTag := false

	for _, r := range html {
		if r == '<' {
			inTag = true
			continue
		}
		if r == '>' {
			inTag = false
			result.WriteRune(' ')
			continue
		}
		if !inTag {
			result.WriteRune(r)
		}
	}

	return result.String()
}

// cleanText normalizes whitespace and trims a text string.
func cleanText(text string) string {
	// Replace common HTML entities
	text = strings.ReplaceAll(text, "&amp;", "&")
	text = strings.ReplaceAll(text, "&lt;", "<")
	text = strings.ReplaceAll(text, "&gt;", ">")
	text = strings.ReplaceAll(text, "&quot;", "\"")
	text = strings.ReplaceAll(text, "&#39;", "'")
	text = strings.ReplaceAll(text, "&nbsp;", " ")

	// Strip any remaining HTML tags
	text = stripTags(text)

	// Normalize whitespace
	fields := strings.Fields(text)
	return strings.Join(fields, " ")
}

// filterByTerms filters intel items to those matching at least one search term.
func filterByTerms(items []IntelItem, terms []string) []IntelItem {
	var filtered []IntelItem

	for _, item := range items {
		text := strings.ToLower(item.Title + " " + item.Content + " " + item.Author)
		for _, term := range terms {
			if strings.Contains(text, strings.ToLower(term)) {
				filtered = append(filtered, item)
				break
			}
		}
	}

	return filtered
}

// generateID creates a short deterministic ID from a string.
func generateID(input string) string {
	hash := sha256.Sum256([]byte(input))
	return fmt.Sprintf("%x", hash[:6])
}

// normalizeURL strips scheme, www prefix, trailing slashes, and tracking
// query params (utm_*, ref, source) to improve cross-source dedup.
// Returns the original string unchanged if it isn't a valid URL.
func normalizeURL(rawURL string) string {
	u, err := url.Parse(rawURL)
	if err != nil || u.Host == "" {
		return rawURL
	}

	// Strip scheme
	host := strings.TrimPrefix(u.Host, "www.")

	// Strip tracking query params
	q := u.Query()
	for key := range q {
		lower := strings.ToLower(key)
		if strings.HasPrefix(lower, "utm_") || lower == "ref" || lower == "source" {
			q.Del(key)
		}
	}

	// Rebuild: host + path (no trailing slash) + cleaned query
	path := strings.TrimRight(u.Path, "/")
	normalized := host + path
	if encoded := q.Encode(); encoded != "" {
		normalized += "?" + encoded
	}

	return strings.ToLower(normalized)
}
