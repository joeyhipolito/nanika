package gather

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"time"
)

// Hacker News Algolia API response structures.
type hnSearchResponse struct {
	Hits []hnHit `json:"hits"`
}

type hnHit struct {
	ObjectID    string   `json:"objectID"`
	Title       string   `json:"title"`
	URL         string   `json:"url"`
	Author      string   `json:"author"`
	Points      int      `json:"points"`
	NumComments int      `json:"num_comments"`
	StoryText   string   `json:"story_text"`
	CreatedAtI  int64    `json:"created_at_i"`
	Tags        []string `json:"_tags"`
}

// HackerNewsGatherer searches Hacker News via the Algolia API.
type HackerNewsGatherer struct {
	Client *http.Client
}

// NewHackerNewsGatherer creates a new Hacker News gatherer.
func NewHackerNewsGatherer() *HackerNewsGatherer {
	return &HackerNewsGatherer{Client: newHTTPClient()}
}

// Name returns the canonical source identifier.
func (g *HackerNewsGatherer) Name() string { return "hackernews" }

// Gather searches HN for stories matching the search terms.
// Searches each term individually for better recall, then deduplicates.
func (g *HackerNewsGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	if len(searchTerms) == 0 {
		return nil, nil
	}
	items, err := collectByTerms(searchTerms, "hackernews", func(term string) ([]IntelItem, error) {
		return g.searchHN(ctx, term)
	})
	if err != nil {
		return nil, err
	}
	return filterByTerms(items, searchTerms), nil
}

// searchHN performs a single Algolia search API call.
// Uses search_by_date for recent content within the last 7 days.
func (g *HackerNewsGatherer) searchHN(ctx context.Context, query string) ([]IntelItem, error) {
	since := time.Now().AddDate(0, 0, -7).Unix()
	params := url.Values{}
	params.Set("query", query)
	params.Set("tags", "story")
	params.Set("numericFilters", fmt.Sprintf("created_at_i>%d", since))
	params.Set("hitsPerPage", "50")
	apiURL := "https://hn.algolia.com/api/v1/search_by_date?" + params.Encode()

	req, err := http.NewRequestWithContext(ctx, "GET", apiURL, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}
	req.Header.Set("User-Agent", userAgent)

	resp, err := g.Client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch HN API: %w", err)
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

	var searchResp hnSearchResponse
	if err := json.Unmarshal(body, &searchResp); err != nil {
		return nil, fmt.Errorf("failed to parse response: %w", err)
	}

	var items []IntelItem
	for _, hit := range searchResp.Hits {
		items = append(items, hnHitToIntel(hit))
	}

	return items, nil
}

// hnHitToIntel converts an HN Algolia hit to an IntelItem.
func hnHitToIntel(hit hnHit) IntelItem {
	sourceURL := hit.URL
	if sourceURL == "" {
		// Ask HN, Show HN, etc. — link to HN discussion
		sourceURL = fmt.Sprintf("https://news.ycombinator.com/item?id=%s", hit.ObjectID)
	}

	content := cleanText(hit.StoryText)
	if len(content) > 500 {
		content = content[:500] + "..."
	}

	var tags []string
	for _, tag := range hit.Tags {
		if tag != "story" && tag != "author_"+hit.Author {
			tags = append(tags, tag)
		}
	}
	tags = append(tags, "hackernews")

	engagement := hit.Points + hit.NumComments

	return IntelItem{
		ID:         generateID(hit.ObjectID),
		Title:      cleanText(hit.Title),
		Content:    content,
		SourceURL:  sourceURL,
		Author:     hit.Author,
		Timestamp:  time.Unix(hit.CreatedAtI, 0).UTC(),
		Tags:       tags,
		Engagement: engagement,
	}
}

// ParseHNResponse parses raw HN API JSON into IntelItems. Exported for testing.
func ParseHNResponse(data []byte) ([]IntelItem, error) {
	var searchResp hnSearchResponse
	if err := json.Unmarshal(data, &searchResp); err != nil {
		return nil, fmt.Errorf("failed to parse HN response: %w", err)
	}

	var items []IntelItem
	for _, hit := range searchResp.Hits {
		items = append(items, hnHitToIntel(hit))
	}
	return items, nil
}
