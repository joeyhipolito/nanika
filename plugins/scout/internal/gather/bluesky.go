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

// blueskySearchResponse is the JSON envelope returned by the Bluesky public search API.
type blueskySearchResponse struct {
	Posts []blueskyPost `json:"posts"`
}

type blueskyPost struct {
	URI    string        `json:"uri"`
	Author blueskyAuthor `json:"author"`
	Record blueskyRecord `json:"record"`
	// Engagement fields
	ReplyCount  int `json:"replyCount"`
	RepostCount int `json:"repostCount"`
	LikeCount   int `json:"likeCount"`
	// Indexed time (when the indexer saw it, available even when createdAt is absent)
	IndexedAt string `json:"indexedAt"`
}

type blueskyAuthor struct {
	Handle      string `json:"handle"`
	DisplayName string `json:"displayName"`
}

type blueskyRecord struct {
	Text      string `json:"text"`
	CreatedAt string `json:"createdAt"`
}

// BlueskyGatherer fetches posts from the Bluesky public API.
// No API key is required; uses api.bsky.app.
type BlueskyGatherer struct {
	Client *http.Client
}

// NewBlueskyGatherer creates a new Bluesky gatherer.
func NewBlueskyGatherer() *BlueskyGatherer {
	return &BlueskyGatherer{
		Client: &http.Client{Timeout: 30 * time.Second},
	}
}

// Gather searches Bluesky for posts matching each search term.
func (g *BlueskyGatherer) Name() string { return "bluesky" }

func (g *BlueskyGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	if len(searchTerms) == 0 {
		return nil, nil
	}

	seen := make(map[string]bool)
	var allItems []IntelItem
	var lastErr error

	for i, term := range searchTerms {
		if i > 0 {
			select {
			case <-time.After(4 * time.Second):
			case <-ctx.Done():
				return nil, ctx.Err()
			}
		}
		items, err := g.searchPosts(ctx, term)
		if err != nil {
			// Retry once on rate limit / forbidden with longer backoff
			if strings.Contains(err.Error(), "403") || strings.Contains(err.Error(), "429") {
				select {
				case <-time.After(10 * time.Second):
				case <-ctx.Done():
					return nil, ctx.Err()
				}
				items, err = g.searchPosts(ctx, term)
			}
		}
		if err != nil {
			lastErr = err
			fmt.Fprintf(os.Stderr, "    Warning: bluesky/%s: %v\n", term, err)
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
		return nil, fmt.Errorf("bluesky: all queries failed, last: %w", lastErr)
	}

	return allItems, nil
}

func (g *BlueskyGatherer) searchPosts(ctx context.Context, query string) ([]IntelItem, error) {
	apiURL := fmt.Sprintf(
		"https://api.bsky.app/xrpc/app.bsky.feed.searchPosts?q=%s&limit=25&sort=latest",
		url.QueryEscape(query),
	)

	req, err := http.NewRequestWithContext(ctx, "GET", apiURL, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}
	req.Header.Set("User-Agent", "scout-cli/0.4 (github.com/joeyhipolito/scout-cli)")

	resp, err := g.Client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch Bluesky API: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusTooManyRequests {
		return nil, fmt.Errorf("rate limited (429)")
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d from Bluesky API", resp.StatusCode)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response: %w", err)
	}

	var result blueskySearchResponse
	if err := json.Unmarshal(body, &result); err != nil {
		return nil, fmt.Errorf("failed to parse response: %w", err)
	}

	var items []IntelItem
	for _, post := range result.Posts {
		items = append(items, blueskyPostToIntel(post))
	}
	return items, nil
}

// blueskyPostToIntel converts a Bluesky post to an IntelItem.
// AT URI: at://did:plc:xxx/app.bsky.feed.post/rkey
// Web URL: https://bsky.app/profile/{handle}/post/{rkey}
func blueskyPostToIntel(post blueskyPost) IntelItem {
	sourceURL := atURIToWebURL(post.URI, post.Author.Handle)

	// Prefer createdAt from the record, fall back to indexedAt
	ts := post.Record.CreatedAt
	if ts == "" {
		ts = post.IndexedAt
	}

	author := post.Author.DisplayName
	if author == "" {
		author = post.Author.Handle
	}

	title := truncateBskyTitle(post.Record.Text, 100)

	return IntelItem{
		ID:         generateID(post.URI),
		Title:      title,
		Content:    post.Record.Text,
		SourceURL:  sourceURL,
		Author:     author,
		Timestamp:  parseDate(ts),
		Tags:       []string{"bluesky"},
		Engagement: post.LikeCount + post.RepostCount,
	}
}

// atURIToWebURL converts an AT Protocol URI to a bsky.app permalink.
// at://did:plc:xxxx/app.bsky.feed.post/rkey → https://bsky.app/profile/{handle}/post/{rkey}
func atURIToWebURL(atURI, handle string) string {
	// at://did:plc:xxx/app.bsky.feed.post/rkey
	after, ok := strings.CutPrefix(atURI, "at://")
	if !ok {
		return atURI
	}
	parts := strings.SplitN(after, "/", 2)
	if len(parts) < 2 {
		return atURI
	}
	// parts[1] is "app.bsky.feed.post/rkey"
	postParts := strings.SplitN(parts[1], "/", 2)
	if len(postParts) < 2 {
		return atURI
	}
	rkey := postParts[1]
	if handle == "" {
		handle = parts[0] // fall back to DID
	}
	return fmt.Sprintf("https://bsky.app/profile/%s/post/%s", handle, rkey)
}

// truncateBskyTitle returns the first maxLen characters of text (whole words),
// appending "..." if truncated. Used for Bluesky post titles.
func truncateBskyTitle(text string, maxLen int) string {
	text = strings.TrimSpace(text)
	if len(text) <= maxLen {
		return text
	}
	// Truncate at last space before maxLen to avoid mid-word cut
	cut := text[:maxLen]
	if idx := strings.LastIndex(cut, " "); idx > 0 {
		cut = cut[:idx]
	}
	return cut + "..."
}
