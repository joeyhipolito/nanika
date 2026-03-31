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

// Reddit JSON API response structures.
type redditResponse struct {
	Data redditData `json:"data"`
}

type redditData struct {
	Children []redditChild `json:"children"`
}

type redditChild struct {
	Data redditPost `json:"data"`
}

type redditPost struct {
	Title       string  `json:"title"`
	Selftext    string  `json:"selftext"`
	Permalink   string  `json:"permalink"`
	URL         string  `json:"url"`
	Author      string  `json:"author"`
	Subreddit   string  `json:"subreddit"`
	Score       int     `json:"score"`
	NumComments int     `json:"num_comments"`
	CreatedUTC  float64 `json:"created_utc"`
}

// RedditGatherer searches Reddit for posts matching search terms.
type RedditGatherer struct {
	Subreddits []string
	Client     *http.Client
}

// NewRedditGatherer creates a new Reddit gatherer with optional subreddit filters.
func NewRedditGatherer(subreddits []string) *RedditGatherer {
	return &RedditGatherer{Subreddits: subreddits, Client: newHTTPClient()}
}

// Name returns the canonical source identifier.
func (g *RedditGatherer) Name() string { return "reddit" }

// Gather searches Reddit for posts matching the search terms.
func (g *RedditGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	if len(searchTerms) == 0 {
		return nil, nil
	}

	query := strings.Join(searchTerms, " OR ")
	seen := make(map[string]bool)
	var all []IntelItem

	// Global search
	if items, err := g.searchReddit(ctx, "", query); err != nil {
		fmt.Fprintf(os.Stderr, "    Warning: reddit global search: %v\n", err)
	} else {
		all = appendUnique(seen, all, items)
	}

	// Per-subreddit search (rate-limited)
	for _, sub := range g.Subreddits {
		select {
		case <-time.After(2 * time.Second):
		case <-ctx.Done():
			return nil, ctx.Err()
		}
		if items, err := g.searchReddit(ctx, sub, query); err != nil {
			fmt.Fprintf(os.Stderr, "    Warning: reddit r/%s: %v\n", sub, err)
		} else {
			all = appendUnique(seen, all, items)
		}
	}

	return filterByTerms(all, searchTerms), nil
}

// searchReddit performs a single Reddit search API call.
// If subreddit is empty, searches globally.
func (g *RedditGatherer) searchReddit(ctx context.Context, subreddit, query string) ([]IntelItem, error) {
	var apiURL string
	if subreddit == "" {
		apiURL = fmt.Sprintf("https://www.reddit.com/search.json?q=%s&sort=relevance&t=month&limit=25",
			url.QueryEscape(query))
	} else {
		apiURL = fmt.Sprintf("https://www.reddit.com/r/%s/search.json?q=%s&restrict_sr=on&sort=relevance&t=month&limit=25",
			url.PathEscape(subreddit), url.QueryEscape(query))
	}

	req, err := http.NewRequestWithContext(ctx, "GET", apiURL, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}

	req.Header.Set("User-Agent", userAgent)

	resp, err := g.Client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch Reddit API: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusTooManyRequests {
		return nil, fmt.Errorf("reddit: rate limited (429). Wait and retry")
	}
	if resp.StatusCode == http.StatusForbidden {
		return nil, fmt.Errorf("reddit: access denied (403)")
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("reddit: HTTP %d", resp.StatusCode)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response: %w", err)
	}

	var redditResp redditResponse
	if err := json.Unmarshal(body, &redditResp); err != nil {
		return nil, fmt.Errorf("failed to parse Reddit response: %w", err)
	}

	var items []IntelItem
	for _, child := range redditResp.Data.Children {
		items = append(items, redditPostToIntel(child.Data))
	}

	return items, nil
}

// redditPostToIntel converts a Reddit post to an IntelItem.
func redditPostToIntel(post redditPost) IntelItem {
	content := cleanText(post.Selftext)
	if len(content) > 500 {
		content = content[:500] + "..."
	}

	sourceURL := "https://www.reddit.com" + post.Permalink

	return IntelItem{
		ID:         generateID(post.Permalink),
		Title:      cleanText(post.Title),
		Content:    content,
		SourceURL:  sourceURL,
		Author:     post.Author,
		Timestamp:  time.Unix(int64(post.CreatedUTC), 0).UTC(),
		Tags:       []string{"r/" + post.Subreddit},
		Engagement: post.Score + post.NumComments,
	}
}
