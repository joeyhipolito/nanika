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

// X API v2 response structures for the recent search endpoint.
type xSearchResponse struct {
	Data     []xTweet        `json:"data"`
	Includes xIncludes       `json:"includes"`
	Meta     xSearchMeta     `json:"meta"`
	Errors   []xAPIError     `json:"errors"`
}

type xTweet struct {
	ID              string         `json:"id"`
	Text            string         `json:"text"`
	AuthorID        string         `json:"author_id"`
	CreatedAt       string         `json:"created_at"`
	PublicMetrics   xPublicMetrics `json:"public_metrics"`
}

type xPublicMetrics struct {
	RetweetCount int `json:"retweet_count"`
	ReplyCount   int `json:"reply_count"`
	LikeCount    int `json:"like_count"`
	QuoteCount   int `json:"quote_count"`
}

type xIncludes struct {
	Users []xUser `json:"users"`
}

type xUser struct {
	ID       string `json:"id"`
	Name     string `json:"name"`
	Username string `json:"username"`
}

type xSearchMeta struct {
	ResultCount int `json:"result_count"`
}

type xAPIError struct {
	Title  string `json:"title"`
	Detail string `json:"detail"`
	Type   string `json:"type"`
}

// XGatherer searches X (formerly Twitter) using the X API v2 recent search endpoint.
type XGatherer struct {
	Client      *http.Client
	BearerToken string
}

// NewXGatherer creates a new X gatherer. It reads the bearer token from the
// X_BEARER_TOKEN environment variable.
func NewXGatherer() *XGatherer {
	return &XGatherer{
		Client:      newHTTPClient(),
		BearerToken: os.Getenv("X_BEARER_TOKEN"),
	}
}

func (g *XGatherer) Name() string { return "x" }

// Gather searches X for tweets matching each search term using the v2 recent search API.
func (g *XGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	if len(searchTerms) == 0 {
		return nil, nil
	}

	if g.BearerToken == "" {
		fmt.Fprintln(os.Stderr, "    Warning: X_BEARER_TOKEN not set, skipping X/Twitter gathering")
		return nil, nil
	}

	seen := make(map[string]bool)
	var allItems []IntelItem
	var lastErr error

	for i, term := range searchTerms {
		if i > 0 {
			select {
			case <-time.After(2 * time.Second):
			case <-ctx.Done():
				return nil, ctx.Err()
			}
		}
		items, err := g.searchTweets(ctx, term)
		if err != nil {
			// Retry once on rate limit with longer backoff
			if strings.Contains(err.Error(), "429") {
				select {
				case <-time.After(15 * time.Second):
				case <-ctx.Done():
					return nil, ctx.Err()
				}
				items, err = g.searchTweets(ctx, term)
			}
		}
		if err != nil {
			lastErr = err
			fmt.Fprintf(os.Stderr, "    Warning: x/%s: %v\n", term, err)
			continue
		}
		for _, item := range items {
			if !seen[item.ID] {
				seen[item.ID] = true
				allItems = append(allItems, item)
			}
		}
	}

	if len(allItems) == 0 && lastErr != nil {
		return nil, fmt.Errorf("x: all queries failed, last: %w", lastErr)
	}

	return allItems, nil
}

func (g *XGatherer) searchTweets(ctx context.Context, query string) ([]IntelItem, error) {
	// Exclude retweets to reduce noise.
	fullQuery := query + " -is:retweet"

	params := url.Values{}
	params.Set("query", fullQuery)
	params.Set("max_results", "25")
	params.Set("tweet.fields", "created_at,public_metrics,author_id")
	params.Set("expansions", "author_id")
	params.Set("user.fields", "name,username")

	apiURL := "https://api.twitter.com/2/tweets/search/recent?" + params.Encode()

	req, err := http.NewRequestWithContext(ctx, "GET", apiURL, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+g.BearerToken)
	req.Header.Set("User-Agent", userAgent)

	resp, err := g.Client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch X API: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusTooManyRequests {
		return nil, fmt.Errorf("rate limited (429)")
	}
	if resp.StatusCode == http.StatusUnauthorized {
		return nil, fmt.Errorf("unauthorized (401): check X_BEARER_TOKEN")
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d from X API", resp.StatusCode)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response: %w", err)
	}

	var result xSearchResponse
	if err := json.Unmarshal(body, &result); err != nil {
		return nil, fmt.Errorf("failed to parse response: %w", err)
	}

	// Build author lookup from includes.
	authors := make(map[string]xUser, len(result.Includes.Users))
	for _, u := range result.Includes.Users {
		authors[u.ID] = u
	}

	var items []IntelItem
	for _, tweet := range result.Data {
		items = append(items, xTweetToIntel(tweet, authors))
	}
	return items, nil
}

// xTweetToIntel converts an X API v2 tweet to an IntelItem.
func xTweetToIntel(tweet xTweet, authors map[string]xUser) IntelItem {
	text := cleanText(tweet.Text)

	// Resolve author from includes lookup.
	author := tweet.AuthorID
	displayAuthor := tweet.AuthorID
	username := ""
	if u, ok := authors[tweet.AuthorID]; ok {
		username = u.Username
		if u.Name != "" {
			displayAuthor = u.Name
		} else {
			displayAuthor = "@" + u.Username
		}
		author = "@" + u.Username
	}

	title := truncateTitle(text, 100)

	// Build tweet URL: https://x.com/{username}/status/{id}
	sourceURL := fmt.Sprintf("https://x.com/i/status/%s", tweet.ID)
	if username != "" {
		sourceURL = fmt.Sprintf("https://x.com/%s/status/%s", username, tweet.ID)
	}

	engagement := tweet.PublicMetrics.LikeCount +
		tweet.PublicMetrics.RetweetCount +
		tweet.PublicMetrics.QuoteCount

	return IntelItem{
		ID:         generateID(sourceURL),
		Title:      displayAuthor + ": " + title,
		Content:    text,
		SourceURL:  sourceURL,
		Author:     author,
		Timestamp:  parseDate(tweet.CreatedAt),
		Tags:       []string{"x"},
		Engagement: engagement,
	}
}

// truncateTitle returns the first maxLen characters of text (whole words),
// appending "..." if truncated.
func truncateTitle(text string, maxLen int) string {
	text = strings.TrimSpace(text)
	if len(text) <= maxLen {
		return text
	}
	cut := text[:maxLen]
	if idx := strings.LastIndex(cut, " "); idx > 0 {
		cut = cut[:idx]
	}
	return cut + "..."
}
