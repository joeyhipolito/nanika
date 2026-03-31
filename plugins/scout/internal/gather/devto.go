package gather

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"
)

// Dev.to API response structure.
type devtoArticle struct {
	ID                     int       `json:"id"`
	Title                  string    `json:"title"`
	Description            string    `json:"description"`
	URL                    string    `json:"url"`
	PublishedAt            string    `json:"published_at"`
	User                   devtoUser `json:"user"`
	Tags                   string    `json:"tags"`
	PositiveReactionsCount int       `json:"positive_reactions_count"`
	CommentsCount          int       `json:"comments_count"`
}

type devtoUser struct {
	Name     string `json:"name"`
	Username string `json:"username"`
}

// DevToGatherer fetches articles from Dev.to by tag or search.
type DevToGatherer struct {
	Tags   []string
	Client *http.Client
}

// NewDevToGatherer creates a new Dev.to gatherer with optional tag filters.
func NewDevToGatherer(tags []string) *DevToGatherer {
	return &DevToGatherer{Tags: tags, Client: newHTTPClient()}
}

// Name returns the canonical source identifier.
func (g *DevToGatherer) Name() string { return "devto" }

// Gather fetches Dev.to articles by tag and search terms.
// Spaces requests 500ms apart to avoid rate limiting.
func (g *DevToGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	seen := make(map[string]bool)
	var all []IntelItem

	// Mode 1: Fetch by configured tags (rate-limited)
	for i, tag := range g.Tags {
		if i > 0 {
			select {
			case <-time.After(500 * time.Millisecond):
			case <-ctx.Done():
				return nil, ctx.Err()
			}
		}
		items, err := g.fetchByTag(ctx, tag)
		if err != nil {
			fmt.Fprintf(os.Stderr, "    Warning: devto/tag/%s: %v\n", tag, err)
			continue
		}
		all = appendUnique(seen, all, items)
	}

	// Mode 2: Supplemental search via latest articles endpoint
	if len(searchTerms) > 0 {
		if len(g.Tags) > 0 {
			select {
			case <-time.After(500 * time.Millisecond):
			case <-ctx.Done():
				return nil, ctx.Err()
			}
		}
		items, err := g.searchArticles(ctx, strings.Join(searchTerms, " "))
		if err != nil {
			fmt.Fprintf(os.Stderr, "    Warning: devto/search: %v\n", err)
		} else {
			all = appendUnique(seen, all, items)
		}
	}

	return filterByTerms(all, searchTerms), nil
}

// fetchByTag fetches top articles for a given tag from the last 7 days.
func (g *DevToGatherer) fetchByTag(ctx context.Context, tag string) ([]IntelItem, error) {
	apiURL := fmt.Sprintf("https://dev.to/api/articles?tag=%s&top=7&per_page=25",
		url.PathEscape(tag))
	return g.fetchDevTo(ctx, apiURL)
}

// searchArticles searches Dev.to articles by query using the latest articles endpoint.
// Dev.to doesn't have a public free-text search API, so we fetch latest articles
// and rely on filterByTerms in Gather to match relevant content.
func (g *DevToGatherer) searchArticles(ctx context.Context, query string) ([]IntelItem, error) {
	apiURL := "https://dev.to/api/articles/latest?per_page=25"
	return g.fetchDevTo(ctx, apiURL)
}

// fetchDevTo performs a Dev.to API call with retry on 429 rate limits.
// Uses exponential backoff: 5s, 15s (base 5s × 3^attempt).
func (g *DevToGatherer) fetchDevTo(ctx context.Context, apiURL string) ([]IntelItem, error) {
	const maxRetries = 2
	backoff := 5 * time.Second

	for attempt := 0; attempt <= maxRetries; attempt++ {
		items, retryable, err := g.doFetchDevTo(ctx, apiURL)
		if err == nil {
			return items, nil
		}
		if !retryable || attempt == maxRetries {
			return nil, err
		}
		fmt.Fprintf(os.Stderr, "    Warning: devto rate limited, retrying in %v...\n", backoff)
		select {
		case <-time.After(backoff):
		case <-ctx.Done():
			return nil, ctx.Err()
		}
		backoff *= 3
	}

	return nil, fmt.Errorf("devto: exhausted retries")
}

// doFetchDevTo performs a single Dev.to API call. Returns (items, retryable, error).
func (g *DevToGatherer) doFetchDevTo(ctx context.Context, apiURL string) ([]IntelItem, bool, error) {
	req, err := http.NewRequestWithContext(ctx, "GET", apiURL, nil)
	if err != nil {
		return nil, false, fmt.Errorf("failed to create request: %w", err)
	}

	req.Header.Set("User-Agent", userAgent)

	if apiKey := os.Getenv("DEVTO_API_KEY"); apiKey != "" {
		req.Header.Set("api-key", apiKey)
	}

	resp, err := g.Client.Do(req)
	if err != nil {
		return nil, false, fmt.Errorf("failed to fetch Dev.to API: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusTooManyRequests {
		return nil, true, fmt.Errorf("rate limited (429)")
	}
	if resp.StatusCode != http.StatusOK {
		return nil, false, fmt.Errorf("HTTP %d", resp.StatusCode)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, false, fmt.Errorf("failed to read response: %w", err)
	}

	var articles []devtoArticle
	if err := json.Unmarshal(body, &articles); err != nil {
		return nil, false, fmt.Errorf("failed to parse response: %w", err)
	}

	var items []IntelItem
	for _, article := range articles {
		items = append(items, devtoArticleToIntel(article))
	}

	return items, false, nil
}

// devtoArticleToIntel converts a Dev.to article to an IntelItem.
func devtoArticleToIntel(article devtoArticle) IntelItem {
	content := cleanText(article.Description)
	if len(content) > 500 {
		content = content[:500] + "..."
	}

	var tags []string
	if article.Tags != "" {
		for _, tag := range strings.Split(article.Tags, ", ") {
			tag = strings.TrimSpace(tag)
			if tag != "" {
				tags = append(tags, tag)
			}
		}
	}
	tags = append(tags, "devto")

	author := article.User.Name
	if author == "" {
		author = article.User.Username
	}

	return IntelItem{
		ID:         generateID(fmt.Sprintf("devto-%d", article.ID)),
		Title:      cleanText(article.Title),
		Content:    content,
		SourceURL:  article.URL,
		Author:     author,
		Timestamp:  parseDate(article.PublishedAt),
		Tags:       tags,
		Engagement: article.PositiveReactionsCount + article.CommentsCount,
	}
}

// ParseDevToResponse parses raw Dev.to API JSON into IntelItems. Exported for testing.
func ParseDevToResponse(data []byte) ([]IntelItem, error) {
	var articles []devtoArticle
	if err := json.Unmarshal(data, &articles); err != nil {
		return nil, fmt.Errorf("failed to parse Dev.to response: %w", err)
	}

	var items []IntelItem
	for _, article := range articles {
		items = append(items, devtoArticleToIntel(article))
	}
	return items, nil
}
